// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! Translation draft generation (design doc §一-3), scoped to external config files
//! (`.ini`/`.xml`). `.NET` DLL-embedded string translation is handled separately by
//! [`crate::dll_translation`] — direct binary patching, not a draft file — after the
//! 2026-08-28 rejection of a hand-rolled Rust PE/CIL parser (on correctness-risk
//! grounds) was superseded once that parser was actually built and verified at
//! production scale (2026-08-30, see that module's doc comment).
//!
//! Uses [`crate::ai_assistant::call_provider`] — the exact same provider-calling code
//! path as [`crate::ai_assistant::diagnose`], so there is only one place in this crate
//! that ever talks to Ollama/cloud endpoints. A draft is always written to a **new
//! sibling file**, never overwriting the original — the design doc is explicit that a
//! draft needs human/community proofreading before it's a real translation, and this
//! project never silently overwrites a mod's own files outside the guarded
//! `install`/`uninstall` write path.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use quick_xml::events::{BytesStart, BytesText, Event};
use quick_xml::Reader;
use rusqlite::Connection;

use crate::error::{CoreError, CoreResult};

const SUPPORTED_EXTENSIONS: &[&str] = &["ini", "xml"];

/// Validates and trims the language tag used in prompts and generated file names.
/// The syntax is restricted to ASCII subtags so it cannot inject path separators or
/// platform-specific file-name syntax.
pub fn validate_target_language(target_language: &str) -> Result<&str, String> {
    let target_language = target_language.trim();
    let mut language_parts = target_language.split('-');
    let primary_language = language_parts.next().unwrap_or_default();
    if target_language.is_empty()
        || target_language.len() > 35
        || !(2..=8).contains(&primary_language.len())
        || !primary_language
            .bytes()
            .all(|character| character.is_ascii_alphabetic())
        || language_parts.any(|part| {
            part.is_empty()
                || part.len() > 8
                || !part
                    .bytes()
                    .all(|character| character.is_ascii_alphanumeric())
        })
    {
        return Err(
            "target language must start with a 2-8 letter code and use optional 1-8 character ASCII subtags"
                .to_string(),
        );
    }
    Ok(target_language)
}

const TRANSLATION_PROMPT_PREFIX_TEMPLATE: &str = "You are translating the human-readable \
    text inside a GTA V mod's configuration file into {target_language}. Preserve every \
    key, tag, attribute name, and structural character exactly as-is — translate only the \
    human-readable text values/labels. Output only the translated file content, nothing \
    else (no commentary, no code fences).\n\nOriginal file content:\n\n";

#[derive(Debug, PartialEq, Eq)]
enum IniStructureEntry {
    Section(String),
    Key {
        section: Option<String>,
        name: String,
        separator: char,
    },
    Continuation,
    Unclassified(String),
}

#[derive(Debug, PartialEq, Eq)]
struct XmlAttributeSignature {
    name: String,
    namespace_binding: Option<Vec<u8>>,
}

#[derive(Debug, PartialEq, Eq)]
enum XmlStructureEntry {
    Start {
        name: String,
        attributes: Vec<XmlAttributeSignature>,
    },
    End(String),
    GeneralReference(Vec<u8>),
    Declaration {
        version: Vec<u8>,
        encoding: Option<Vec<u8>>,
        standalone: Option<Vec<u8>>,
    },
    DocType(Vec<u8>),
    ProcessingInstruction(Vec<u8>),
}

// OpenAI-Codex: reject configuration drafts that alter their source structure.
fn validate_translation_structure(
    source_path: &Path,
    source: &str,
    translated: &str,
) -> CoreResult<()> {
    let extension = source_path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();

    let reason = match extension.as_str() {
        "ini" => {
            let source_structure = ini_structure_signature(source);
            let translated_structure = ini_structure_signature(translated);
            (source_structure != translated_structure)
                .then_some(
                    "AI translation changed INI sections, keys, or syntax; indented assignments and section headers remain structural",
                )
        }
        "xml" => {
            let source_structure =
                xml_structure_signature(source).map_err(|reason| CoreError::AiAssistant {
                    reason: format!("source XML is not structurally valid: {reason}"),
                })?;
            let translated_structure =
                xml_structure_signature(translated).map_err(|reason| CoreError::AiAssistant {
                    reason: format!("translated XML is not structurally valid: {reason}"),
                })?;
            (source_structure != translated_structure)
                .then_some("AI translation changed XML element or attribute structure")
        }
        _ => Some("translation draft validation only supports .ini/.xml files"),
    };

    if let Some(reason) = reason {
        return Err(CoreError::AiAssistant {
            reason: reason.to_string(),
        });
    }

    Ok(())
}

fn validate_xml_source_before_provider(source_path: &Path, source: &str) -> CoreResult<()> {
    let is_xml = source_path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("xml"));
    if !is_xml {
        return Ok(());
    }

    xml_structure_signature(source)
        .map(|_| ())
        .map_err(|reason| CoreError::AiAssistant {
            reason: format!("source XML is not structurally valid: {reason}"),
        })
}

/// Captures INI sections and assignment keys while ignoring translatable values and comments.
/// Unknown non-comment syntax is retained verbatim so the provider cannot silently rewrite it.
/// For unknown INI dialects, key and section syntax remains structural even when indented.
fn ini_structure_signature(content: &str) -> Vec<IniStructureEntry> {
    let mut entries = Vec::new();
    let mut current_section = None;
    let mut value_can_continue = false;

    for (line_index, line) in content.lines().enumerate() {
        let line = if line_index == 0 {
            line.strip_prefix('\u{feff}').unwrap_or(line)
        } else {
            line
        };
        let is_indented = line.starts_with(' ') || line.starts_with('\t');
        let line = line.trim();
        if line.is_empty() {
            value_can_continue = false;
            continue;
        }
        if line.starts_with(';') || line.starts_with('#') {
            value_can_continue = false;
            continue;
        }

        if let Some(section_text) = line.strip_prefix('[') {
            if let Some(close_bracket) = section_text.find(']') {
                let name = section_text[..close_bracket].trim();
                let trailing = section_text[close_bracket + 1..].trim();
                if !name.is_empty()
                    && (trailing.is_empty()
                        || trailing.starts_with(';')
                        || trailing.starts_with('#'))
                {
                    let name = name.to_string();
                    entries.push(IniStructureEntry::Section(name.clone()));
                    current_section = Some(name);
                    value_can_continue = false;
                    continue;
                }
            }

            entries.push(IniStructureEntry::Unclassified(line.to_string()));
            value_can_continue = false;
            continue;
        }

        if let Some(separator_index) = line.find(|character| matches!(character, '=' | ':')) {
            let name = line[..separator_index].trim();
            if !name.is_empty() {
                entries.push(IniStructureEntry::Key {
                    section: current_section.clone(),
                    name: name.to_string(),
                    separator: line[separator_index..].chars().next().unwrap_or('='),
                });
                value_can_continue = true;
                continue;
            }
        }

        if is_indented && value_can_continue {
            entries.push(IniStructureEntry::Continuation);
            continue;
        }

        entries.push(IniStructureEntry::Unclassified(line.to_string()));
        value_can_continue = false;
    }

    entries
}

fn decode_xml_name(name: &[u8]) -> Result<String, String> {
    std::str::from_utf8(name)
        .map(str::to_string)
        .map_err(|error| format!("element or attribute name is not UTF-8: {error}"))
}

fn xml_element_signature(
    tag: &BytesStart<'_>,
) -> Result<(String, Vec<XmlAttributeSignature>), String> {
    let name = decode_xml_name(tag.name().as_ref())?;
    let mut attributes = Vec::new();

    for attribute in tag.attributes().with_checks(true) {
        let attribute = attribute.map_err(|error| format!("invalid XML attribute: {error}"))?;
        let key = attribute.key.as_ref();
        let is_namespace_binding = key == b"xmlns" || key.starts_with(b"xmlns:");
        attributes.push(XmlAttributeSignature {
            name: decode_xml_name(key)?,
            namespace_binding: is_namespace_binding.then(|| attribute.value.as_ref().to_vec()),
        });
    }

    // XML attribute order does not change document structure, but namespace bindings do.
    attributes.sort_unstable_by(|left, right| left.name.cmp(&right.name));
    Ok((name, attributes))
}

// OpenAI-Codex_Responsible: Source_Code: preserve custom entity references in 0.36.2 text events.
fn append_xml_general_references(
    text: &BytesText<'_>,
    entries: &mut Vec<XmlStructureEntry>,
) -> Result<(), String> {
    // The resolved value is discarded; a static value keeps the 0.36.2 resolver lifetime-safe.
    text.unescape_with(|_entity| Some(""))
        .map_err(|error| format!("invalid XML text reference: {error}"))?;

    let mut remaining = text.as_ref();
    while let Some(ampersand_index) = remaining.iter().position(|byte| *byte == b'&') {
        let after_ampersand = &remaining[ampersand_index + 1..];
        let semicolon_index = after_ampersand
            .iter()
            .position(|byte| *byte == b';')
            .ok_or_else(|| "unterminated XML entity reference".to_string())?;
        let reference_name = &after_ampersand[..semicolon_index];
        if reference_name.is_empty() {
            return Err("empty XML entity reference".to_string());
        }
        let is_predefined_reference = reference_name == b"amp"
            || reference_name == b"lt"
            || reference_name == b"gt"
            || reference_name == b"apos"
            || reference_name == b"quot";
        if !reference_name.starts_with(b"#") && !is_predefined_reference {
            entries.push(XmlStructureEntry::GeneralReference(reference_name.to_vec()));
        }
        remaining = &after_ampersand[semicolon_index + 1..];
    }

    Ok(())
}

/// Parses a complete XML document and captures its ordered element/attribute structure.
/// Text and attribute values are intentionally ignored because they are translation targets.
fn xml_structure_signature(content: &str) -> Result<Vec<XmlStructureEntry>, String> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut reader = Reader::from_str(content);
    let mut entries = Vec::new();
    let mut open_elements = Vec::new();
    let mut root_seen = false;
    let mut declaration_seen = false;
    let mut declaration_allowed = true;
    let mut doctype_seen = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(tag)) => {
                if open_elements.is_empty() {
                    if root_seen {
                        return Err("multiple root elements".to_string());
                    }
                    root_seen = true;
                }

                let (name, attributes) = xml_element_signature(&tag)?;
                entries.push(XmlStructureEntry::Start {
                    name: name.clone(),
                    attributes,
                });
                open_elements.push(name);
            }
            Ok(Event::Empty(tag)) => {
                if open_elements.is_empty() {
                    if root_seen {
                        return Err("multiple root elements".to_string());
                    }
                    root_seen = true;
                }

                let (name, attributes) = xml_element_signature(&tag)?;
                entries.push(XmlStructureEntry::Start {
                    name: name.clone(),
                    attributes,
                });
                entries.push(XmlStructureEntry::End(name));
            }
            Ok(Event::End(tag)) => {
                let name = decode_xml_name(tag.name().as_ref())?;
                if open_elements.pop().as_deref() != Some(name.as_str()) {
                    return Err(format!("mismatched or unexpected closing element: {name}"));
                }
                entries.push(XmlStructureEntry::End(name));
            }
            Ok(Event::Text(text)) => {
                if open_elements.is_empty() {
                    if text
                        .as_ref()
                        .iter()
                        .any(|byte| !matches!(byte, b' ' | b'\t' | b'\n' | b'\r'))
                    {
                        return Err(
                            "non-whitespace text appears outside the root element".to_string()
                        );
                    }
                    declaration_allowed = false;
                } else {
                    append_xml_general_references(&text, &mut entries)?;
                }
            }
            Ok(Event::CData(_)) if open_elements.is_empty() => {
                return Err("content appears outside the root element".to_string());
            }
            Ok(Event::Decl(declaration)) => {
                if root_seen || declaration_seen || !declaration_allowed {
                    return Err("XML declaration is misplaced or repeated".to_string());
                }
                declaration_seen = true;
                let version = declaration
                    .version()
                    .map_err(|error| format!("invalid XML declaration: {error}"))?
                    .into_owned();
                let encoding = declaration
                    .encoding()
                    .transpose()
                    .map_err(|error| format!("invalid XML declaration: {error}"))?
                    .map(|value| value.into_owned());
                let standalone = declaration
                    .standalone()
                    .transpose()
                    .map_err(|error| format!("invalid XML declaration: {error}"))?
                    .map(|value| value.into_owned());
                if encoding
                    .as_deref()
                    .is_some_and(|encoding| !encoding.eq_ignore_ascii_case(b"UTF-8"))
                {
                    return Err("only UTF-8 XML encoding declarations are supported".to_string());
                }
                entries.push(XmlStructureEntry::Declaration {
                    version,
                    encoding,
                    standalone,
                });
            }
            Ok(Event::DocType(doctype)) => {
                if root_seen || doctype_seen {
                    return Err("DOCTYPE is misplaced or repeated".to_string());
                }
                doctype_seen = true;
                declaration_allowed = false;
                entries.push(XmlStructureEntry::DocType(doctype.as_ref().to_vec()));
            }
            Ok(Event::PI(instruction)) => {
                if open_elements.is_empty() {
                    declaration_allowed = false;
                }
                entries.push(XmlStructureEntry::ProcessingInstruction(
                    instruction.as_ref().to_vec(),
                ));
            }
            Ok(Event::Comment(_)) if open_elements.is_empty() => {
                declaration_allowed = false;
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(format!("XML parse error: {error}")),
        }
    }

    if !open_elements.is_empty() {
        return Err("one or more XML elements are not closed".to_string());
    }
    if !root_seen {
        return Err("XML document has no root element".to_string());
    }

    Ok(entries)
}

fn write_translation_draft(
    source_path: &Path,
    target_language: &str,
    source: &str,
    translated: &str,
) -> CoreResult<PathBuf> {
    let target_language = validate_target_language(target_language)
        .map_err(|reason| CoreError::AiAssistant { reason })?;
    validate_translation_structure(source_path, source, translated)?;

    let output_path = sibling_draft_path(source_path, target_language)?;
    // OpenAI-Codex: create_new closes the preflight/write race and preserves old drafts.
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output_path)?;
    output.write_all(translated.as_bytes())?;
    Ok(output_path)
}

/// Generates a translation draft for `source_path` (must be `.ini` or `.xml`) into
/// `target_language`, writing it to a new sibling file
/// (`name.ext` -> `name.<target_language>.ext`) and returning that path. The original
/// file is never touched. Requires [`crate::ai_assistant::enable`] to have been called
/// first (same provider gating as [`crate::ai_assistant::diagnose`]).
pub fn generate_draft(
    conn: &Connection,
    source_path: &Path,
    target_language: &str,
) -> CoreResult<PathBuf> {
    let target_language = validate_target_language(target_language)
        .map_err(|reason| CoreError::AiAssistant { reason })?;
    let ext = source_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();
    if !SUPPORTED_EXTENSIONS.contains(&ext.as_str()) {
        return Err(CoreError::AiAssistant {
            reason: format!(
                "translation draft generation only supports .ini/.xml files, got: {}",
                source_path.display()
            ),
        });
    }

    let content = std::fs::read_to_string(source_path)?;
    validate_xml_source_before_provider(source_path, &content)?;
    let prompt = format!(
        "{}{content}",
        TRANSLATION_PROMPT_PREFIX_TEMPLATE.replace("{target_language}", target_language)
    );

    let draft = crate::ai_assistant::call_provider(conn, &prompt)?;
    write_translation_draft(source_path, target_language, &content, &draft)
}

fn sibling_draft_path(source_path: &Path, target_language: &str) -> CoreResult<PathBuf> {
    let target_language = validate_target_language(target_language)
        .map_err(|reason| CoreError::AiAssistant { reason })?;
    let stem = source_path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| CoreError::AiAssistant {
            reason: format!(
                "could not determine a file name for {}",
                source_path.display()
            ),
        })?;
    let ext = source_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let file_name = format!("{stem}.{target_language}.{ext}");
    Ok(source_path.with_file_name(file_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_target_language_trims_and_accepts_language_tags() {
        assert_eq!(validate_target_language(" zh-TW ").unwrap(), "zh-TW");
        assert_eq!(
            validate_target_language("sr-Latn-RS").unwrap(),
            "sr-Latn-RS"
        );
    }

    #[test]
    fn validate_target_language_rejects_path_syntax_and_invalid_subtags() {
        for candidate in [
            "../outside",
            r"..\outside",
            "en/US",
            r"en\US",
            "/tmp/zh-TW",
            "x",
            "zh--TW",
        ] {
            assert!(
                validate_target_language(candidate).is_err(),
                "accepted unsafe or invalid target language: {candidate}"
            );
        }
    }

    #[test]
    fn sibling_draft_path_rejects_path_syntax_in_target_language() {
        for candidate in ["../outside", r"..\outside", "en/US", r"en\US"] {
            assert!(
                sibling_draft_path(Path::new("C:/mods/outfit.xml"), candidate).is_err(),
                "accepted unsafe target language: {candidate}"
            );
        }
    }

    #[test]
    fn refuses_unsupported_extensions_before_touching_the_ai_provider() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mod.dll");
        std::fs::write(&path, b"not a config file").unwrap();

        let conn = crate::db::open_in_memory().unwrap();
        let err = generate_draft(&conn, &path, "zh-TW").unwrap_err();
        assert!(matches!(err, CoreError::AiAssistant { .. }));
    }

    #[test]
    fn refuses_when_ai_assistant_is_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mod.ini");
        std::fs::write(&path, b"[Settings]\nLabel=Hello").unwrap();

        let conn = crate::db::open_in_memory().unwrap();
        let err = generate_draft(&conn, &path, "zh-TW").unwrap_err();
        assert!(matches!(err, CoreError::AiAssistant { .. }));
    }

    #[test]
    fn rejects_malformed_xml_before_consulting_the_ai_provider() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mod.xml");
        std::fs::write(&path, "<Settings>").unwrap();

        let conn = crate::db::open_in_memory().unwrap();
        let err = generate_draft(&conn, &path, "zh-TW").unwrap_err();
        let CoreError::AiAssistant { reason } = err else {
            panic!("expected an XML validation error");
        };

        assert!(reason.contains("source XML is not structurally valid"));
        assert!(!reason.contains("AI assistant is disabled"));
        assert!(!dir.path().join("mod.zh-TW.xml").exists());
    }

    #[test]
    fn rejects_non_utf8_xml_encoding_before_consulting_the_ai_provider() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mod.xml");
        let source = r#"<?xml version="1.0" encoding="ISO-8859-1"?><Settings><Label>Hello</Label></Settings>"#;
        std::fs::write(&path, source).unwrap();

        let conn = crate::db::open_in_memory().unwrap();
        let err = generate_draft(&conn, &path, "zh-TW").unwrap_err();
        let CoreError::AiAssistant { reason } = err else {
            panic!("expected an XML encoding validation error");
        };

        assert!(reason.contains("UTF-8"));
        assert!(!reason.contains("AI assistant is disabled"));
        assert!(!dir.path().join("mod.zh-TW.xml").exists());
    }

    #[test]
    fn sibling_draft_path_inserts_the_target_language_before_the_extension() {
        let path = Path::new("C:/mods/outfit.xml");
        let draft = sibling_draft_path(path, "zh-TW").unwrap();
        assert_eq!(draft, Path::new("C:/mods/outfit.zh-TW.xml"));
    }

    #[test]
    fn never_writes_to_the_original_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mod.ini");
        std::fs::write(&path, b"[Settings]\nLabel=Hello").unwrap();
        let draft = sibling_draft_path(&path, "zh-TW").unwrap();
        assert_ne!(draft, path);
    }

    #[test]
    fn ini_structure_allows_value_translation_and_preserves_sections_and_keys() {
        let source = "[Settings]\nLabel=Hello\nEnabled=true\n";
        let translated = "[Settings]\nLabel=你好\nEnabled=true\n";

        assert!(validate_translation_structure(Path::new("mod.ini"), source, translated).is_ok());
    }

    #[test]
    fn ini_structure_rejects_changed_sections_or_keys() {
        let source = "[Settings]\nLabel=Hello\n";
        let translated = "[設定]\n標籤=你好\n";

        assert!(validate_translation_structure(Path::new("mod.ini"), source, translated).is_err());
    }

    #[test]
    fn ini_structure_allows_translated_indented_value_continuations() {
        let source = "[Strings]\nMessage=Hello\n  next line\n";
        let translated = "[Strings]\nMessage=你好\n  下一行\n";

        assert!(validate_translation_structure(Path::new("mod.ini"), source, translated).is_ok());
    }

    #[test]
    fn ini_structure_allows_indented_assignment_value_translation_when_key_is_preserved() {
        let source = "[Strings]\n  Label=Hello\n";
        let translated = "[Strings]\n  Label=你好\n";

        assert!(validate_translation_structure(Path::new("mod.ini"), source, translated).is_ok());
    }

    #[test]
    fn ini_structure_rejects_translated_key_like_indented_lines() {
        let source = "[Strings]\nMessage=Intro\n  URL: https://example.test/path=C:\\Mods\n";
        let translated = "[Strings]\nMessage=介紹\n  網址： https://example.test/path=C:\\Mods\n";

        assert!(validate_translation_structure(Path::new("mod.ini"), source, translated).is_err());
    }

    #[test]
    fn ini_structure_rejects_translated_indented_section_names() {
        let source = "[Strings]\nMessage=Intro\n  [Options]\n";
        let translated = "[Strings]\nMessage=介紹\n  [選項]\n";

        assert!(validate_translation_structure(Path::new("mod.ini"), source, translated).is_err());
    }

    #[test]
    fn xml_structure_allows_text_translation_attribute_value_changes_and_empty_tag_normalization() {
        let source = r#"<Settings lang="en"><Label key="name">Hello</Label><Flag /></Settings>"#;
        let translated =
            r#"<Settings lang="zh"><Label key="name">你好</Label><Flag></Flag></Settings>"#;

        assert!(validate_translation_structure(Path::new("mod.xml"), source, translated).is_ok());
    }

    #[test]
    fn xml_structure_rejects_malformed_xml_and_changed_element_or_attribute_names() {
        let source = r#"<Settings><Label key="name">Hello</Label></Settings>"#;
        let malformed = r#"<Settings><Label key="name">你好</Settings>"#;
        let changed_element = r#"<Settings><Title key="name">你好</Title></Settings>"#;
        let changed_attribute = r#"<Settings><Label id="name">你好</Label></Settings>"#;

        assert!(validate_translation_structure(Path::new("mod.xml"), source, malformed).is_err());
        assert!(
            validate_translation_structure(Path::new("mod.xml"), source, changed_element).is_err()
        );
        assert!(
            validate_translation_structure(Path::new("mod.xml"), source, changed_attribute)
                .is_err()
        );
    }

    #[test]
    fn xml_structure_rejects_changed_namespace_binding() {
        let source = r#"<r:Settings xmlns:r="urn:source"><r:Label>Hello</r:Label></r:Settings>"#;
        let translated =
            r#"<r:Settings xmlns:r="urn:translated"><r:Label>你好</r:Label></r:Settings>"#;

        assert!(validate_translation_structure(Path::new("mod.xml"), source, translated).is_err());
    }

    #[test]
    fn xml_structure_rejects_changed_general_entity_reference() {
        let source = "<!DOCTYPE root [<!ENTITY field \"<Label>Hello</Label>\"><!ENTITY other \"<Other>Hello</Other>\">]><root>&field;</root>";
        let translated = "<!DOCTYPE root [<!ENTITY field \"<Label>Hello</Label>\"><!ENTITY other \"<Other>Hello</Other>\">]><root>&other;</root>";

        assert!(validate_translation_structure(Path::new("mod.xml"), source, translated).is_err());
    }

    #[test]
    fn xml_structure_allows_text_translation_around_an_unchanged_general_entity_reference() {
        let source = "<!DOCTYPE root [<!ENTITY field \"fixed\">]><root>Loading &field; now.</root>";
        let translated =
            "<!DOCTYPE root [<!ENTITY field \"fixed\">]><root>正在載入 &field;。</root>";

        assert!(validate_translation_structure(Path::new("mod.xml"), source, translated).is_ok());
    }

    #[test]
    fn xml_structure_allows_equivalent_numeric_character_references() {
        let source = "<Settings>&#x4E2D;&amp;</Settings>";
        let translated = "<Settings>中&amp;</Settings>";

        assert!(validate_translation_structure(Path::new("mod.xml"), source, translated).is_ok());
    }

    #[test]
    fn xml_structure_rejects_declaration_after_prolog_comment() {
        let source = "<!--comment--><?xml version=\"1.0\"?><Settings/>";

        assert!(validate_translation_structure(Path::new("mod.xml"), source, source).is_err());
    }

    #[test]
    fn xml_structure_rejects_changed_declaration_encoding() {
        let source =
            r#"<?xml version="1.0" encoding="UTF-8"?><Settings><Label>Hello</Label></Settings>"#;
        let translated = r#"<?xml version="1.0" encoding="ISO-8859-1"?><Settings><Label>你好</Label></Settings>"#;

        assert!(validate_translation_structure(Path::new("mod.xml"), source, translated).is_err());
    }

    #[test]
    fn xml_structure_accepts_lowercase_utf8_declaration() {
        let source =
            r#"<?xml version="1.0" encoding="utf-8"?><Settings><Label>Hello</Label></Settings>"#;
        let translated =
            r#"<?xml version="1.0" encoding="utf-8"?><Settings><Label>你好</Label></Settings>"#;

        assert!(validate_translation_structure(Path::new("mod.xml"), source, translated).is_ok());
    }

    #[test]
    fn rejects_ini_structure_changes_before_creating_a_draft() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("mod.ini");
        let source = "[Settings]\nLabel=Hello\n";
        std::fs::write(&source_path, source).unwrap();

        let result = write_translation_draft(&source_path, "zh-TW", source, "[設定]\n標籤=你好\n");

        assert!(matches!(result, Err(CoreError::AiAssistant { .. })));
        assert!(!dir.path().join("mod.zh-TW.ini").exists());
        assert_eq!(std::fs::read_to_string(&source_path).unwrap(), source);
    }

    #[test]
    fn rejects_malformed_xml_before_creating_a_draft() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("mod.xml");
        let source = "<Settings><Label>Hello</Label></Settings>";
        std::fs::write(&source_path, source).unwrap();

        let result = write_translation_draft(
            &source_path,
            "zh-TW",
            source,
            "<Settings><Label>你好</Settings>",
        );

        assert!(matches!(result, Err(CoreError::AiAssistant { .. })));
        assert!(!dir.path().join("mod.zh-TW.xml").exists());
        assert_eq!(std::fs::read_to_string(&source_path).unwrap(), source);
    }
}

// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
// OpenAI-Codex
// OpenAI-Codex_dlc_deep_audit

//! Pure validation and item comparison for user-supplied dlclist XML drafts.
//! This module never opens, rewrites, imports, or confirms registration of a file.

use std::collections::BTreeMap;

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::error::{CoreError, CoreResult};

pub const MAX_DLCLIST_BYTES: usize = 5 * 1024 * 1024;
pub const MAX_DLCLIST_DEPTH: usize = 64;

#[derive(Debug, Clone, Serialize)]
pub struct DlcEditorValidation {
    pub entries: Vec<String>,
    pub duplicate_entries: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DlcEditorDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub reordered: bool,
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn invalid(reason: &str) -> CoreError {
    CoreError::UnsupportedFormat {
        reason: format!("dlclist XML: {reason}"),
    }
}

fn require(condition: bool, reason: &str) -> CoreResult<()> {
    if condition {
        Ok(())
    } else {
        Err(invalid(reason))
    }
}

fn xml_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n')
}

fn valid_char(c: char) -> bool {
    matches!(c as u32, 0x9 | 0xa | 0xd | 0x20..=0xd7ff | 0xe000..=0xfffd | 0x10000..=0x10ffff)
}

fn name_start(c: char) -> bool {
    c == '_'
        || c.is_ascii_alphabetic()
        || matches!(c as u32,
        0xc0..=0xd6 | 0xd8..=0xf6 | 0xf8..=0x2ff | 0x370..=0x37d |
        0x37f..=0x1fff | 0x200c..=0x200d | 0x2070..=0x218f |
        0x2c00..=0x2fef | 0x3001..=0xd7ff | 0xf900..=0xfdcf |
        0xfdf0..=0xfffd | 0x10000..=0xeffff)
}

fn checked_name(bytes: &[u8]) -> CoreResult<String> {
    let name = std::str::from_utf8(bytes).map_err(|_| invalid("name is not UTF-8"))?;
    let mut chars = name.chars();
    require(
        chars.next().is_some_and(name_start)
            && chars.all(|c| {
                name_start(c)
                    || c.is_ascii_digit()
                    || matches!(c, '-' | '.' | '\u{b7}')
                    || matches!(c as u32, 0x300..=0x36f | 0x203f..=0x2040)
            }),
        "invalid or unsupported element/attribute name (namespaces are unsupported)",
    )?;
    Ok(name.to_string())
}

fn decoded(bytes: &[u8]) -> CoreResult<String> {
    let raw = std::str::from_utf8(bytes).map_err(|_| invalid("text is not UTF-8"))?;
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    let value = quick_xml::escape::unescape(&normalized)
        .map_err(|_| invalid("invalid or custom entity reference"))?;
    require(
        value.chars().all(valid_char),
        "invalid XML character reference",
    )?;
    Ok(value.into_owned())
}

fn checked_tag(tag: &BytesStart<'_>) -> CoreResult<String> {
    let name = checked_name(tag.name().as_ref())?;
    // quick_xml handles attribute parsing and duplicates. Also require the XML
    // grammar's whitespace separators; do not accept adjacent quoted values.
    let bytes = tag.as_ref();
    let mut offset = tag.name().as_ref().len();
    while offset < bytes.len() {
        require(
            bytes[offset].is_ascii_whitespace(),
            "attributes require whitespace separators",
        )?;
        while offset < bytes.len() && bytes[offset].is_ascii_whitespace() {
            offset += 1;
        }
        if offset == bytes.len() {
            break;
        }
        let equals = bytes[offset..]
            .iter()
            .position(|b| *b == b'=')
            .ok_or_else(|| invalid("attribute lacks '='"))?;
        offset += equals + 1;
        while offset < bytes.len() && bytes[offset].is_ascii_whitespace() {
            offset += 1;
        }
        let quote = *bytes
            .get(offset)
            .ok_or_else(|| invalid("attribute lacks a quoted value"))?;
        require(
            matches!(quote, b'\'' | b'"'),
            "attribute values must be quoted",
        )?;
        offset += 1;
        let end = bytes[offset..]
            .iter()
            .position(|b| *b == quote)
            .ok_or_else(|| invalid("unterminated attribute value"))?;
        offset += end + 1;
    }
    for attribute in tag.attributes().with_checks(true) {
        let attribute = attribute.map_err(|_| invalid("invalid or duplicate attribute"))?;
        let key = checked_name(attribute.key.as_ref())?;
        require(key != "xmlns", "namespace declarations are unsupported")?;
        require(
            !attribute.value.contains(&b'<'),
            "literal '<' inside an attribute",
        )?;
        decoded(attribute.value.as_ref())?;
    }
    Ok(name)
}

#[derive(Default)]
struct Parser {
    stack: Vec<String>,
    root_seen: bool,
    paths_seen: bool,
    item: Option<String>,
    entries: Vec<String>,
}

impl Parser {
    fn start(&mut self, tag: &BytesStart<'_>, empty: bool) -> CoreResult<()> {
        let name = checked_tag(tag)?;
        require(
            self.stack.len() < MAX_DLCLIST_DEPTH,
            "nesting exceeds the supported depth",
        )?;
        require(self.item.is_none(), "Item must contain character data only")?;
        if self.stack.is_empty() {
            require(
                !self.root_seen && name == "SMandatoryPacksData",
                "exactly one SMandatoryPacksData root is required",
            )?;
            self.root_seen = true;
        }
        if name == "Paths" {
            require(
                self.stack.len() == 1 && !self.paths_seen,
                "exactly one direct Paths child is required",
            )?;
            self.paths_seen = true;
        }
        if self.stack.last().is_some_and(|name| name == "Paths") {
            require(name == "Item", "Paths may contain only Item elements")?;
        }
        if name == "Item" {
            require(
                self.stack.len() == 2 && self.stack.last().is_some_and(|name| name == "Paths"),
                "Item must be a direct Paths child",
            )?;
            require(!empty, "Item cannot be empty")?;
            self.item = Some(String::new());
        }
        if !empty {
            self.stack.push(name);
        }
        Ok(())
    }

    fn end(&mut self, bytes: &[u8]) -> CoreResult<()> {
        let name = checked_name(bytes)?;
        require(
            self.stack.pop().as_deref() == Some(name.as_str()),
            "mismatched or unexpected closing tag",
        )?;
        if name == "Item" {
            let item = self
                .item
                .take()
                .ok_or_else(|| invalid("Item has no text context"))?;
            let item = item.trim_matches(xml_space);
            require(!item.is_empty(), "Item cannot be empty")?;
            self.entries.push(item.to_string());
        }
        Ok(())
    }

    fn text(&mut self, value: &str) -> CoreResult<()> {
        if let Some(item) = &mut self.item {
            item.push_str(value);
        } else if self.stack.is_empty() || self.stack.last().is_some_and(|name| name == "Paths") {
            require(
                value.chars().all(xml_space),
                "unexpected text outside an Item or root",
            )?;
        }
        Ok(())
    }
}

/// Validate only the supported document form, retaining every decoded Item value.
/// Namespace syntax and DTDs are rejected rather than partially interpreted.
pub fn validate_dlclist(content: &str) -> CoreResult<DlcEditorValidation> {
    require(
        content.len() <= MAX_DLCLIST_BYTES,
        "document exceeds the 5 MiB limit",
    )?;
    require(
        content.chars().all(valid_char),
        "document contains invalid XML characters",
    )?;
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut reader = Reader::from_str(content);
    let mut parser = Parser::default();
    let mut declaration_allowed = true;
    loop {
        let event = reader
            .read_event()
            .map_err(|_| invalid("malformed XML syntax"))?;
        match event {
            Event::Start(tag) => {
                parser.start(&tag, false)?;
                declaration_allowed = false;
            }
            Event::Empty(tag) => {
                parser.start(&tag, true)?;
                declaration_allowed = false;
            }
            Event::End(tag) => {
                parser.end(tag.name().as_ref())?;
                declaration_allowed = false;
            }
            Event::Text(text) => {
                require(
                    !text.as_ref().windows(3).any(|bytes| bytes == b"]]>"),
                    "literal ']]>' outside CDATA",
                )?;
                // XML character references are legal inside elements, not as
                // substitutes for grammar whitespace before/after the root.
                if parser.stack.is_empty() {
                    require(
                        text.as_ref()
                            .iter()
                            .all(|c| matches!(*c, b' ' | b'\t' | b'\r' | b'\n')),
                        "text outside root",
                    )?;
                }
                parser.text(&decoded(text.as_ref())?)?;
                declaration_allowed = false;
            }
            Event::CData(text) => {
                require(!parser.stack.is_empty(), "CDATA outside root")?;
                let value = std::str::from_utf8(text.as_ref())
                    .map_err(|_| invalid("CDATA is not UTF-8"))?;
                parser.text(&value.replace("\r\n", "\n").replace('\r', "\n"))?;
                declaration_allowed = false;
            }
            Event::Comment(comment) => {
                require(
                    parser.item.is_none(),
                    "comments inside Item are unsupported",
                )?;
                let bytes = comment.as_ref();
                require(
                    !bytes.windows(2).any(|b| b == b"--") && !bytes.ends_with(b"-"),
                    "invalid XML comment",
                )?;
                declaration_allowed = false;
            }
            Event::PI(instruction) => {
                require(
                    parser.item.is_none(),
                    "processing instructions inside Item are unsupported",
                )?;
                let raw = std::str::from_utf8(instruction.as_ref())
                    .map_err(|_| invalid("instruction is not UTF-8"))?;
                let target = raw.split(xml_space).next().unwrap_or_default();
                checked_name(target.as_bytes())?;
                require(
                    !target.eq_ignore_ascii_case("xml"),
                    "reserved processing instruction target",
                )?;
                declaration_allowed = false;
            }
            Event::Decl(declaration) => {
                require(declaration_allowed, "misplaced or repeated XML declaration")?;
                require(
                    declaration
                        .version()
                        .map_err(|_| invalid("invalid XML declaration"))?
                        .as_ref()
                        == b"1.0",
                    "only XML 1.0 is supported",
                )?;
                let encoding = declaration
                    .encoding()
                    .transpose()
                    .map_err(|_| invalid("invalid encoding declaration"))?;
                require(
                    encoding
                        .as_ref()
                        .is_none_or(|e| e.eq_ignore_ascii_case(b"UTF-8")),
                    "only UTF-8 encoding is supported",
                )?;
                let standalone = declaration
                    .standalone()
                    .transpose()
                    .map_err(|_| invalid("invalid standalone declaration"))?;
                require(
                    standalone
                        .as_ref()
                        .is_none_or(|s| s.as_ref() == b"yes" || s.as_ref() == b"no"),
                    "invalid standalone declaration",
                )?;
                let raw = std::str::from_utf8(declaration.as_ref())
                    .map_err(|_| invalid("declaration is not UTF-8"))?;
                let tag = BytesStart::from_content(raw, 3);
                checked_tag(&tag)?;
                let keys = tag
                    .attributes()
                    .map(|a| {
                        a.map(|a| a.key.as_ref().to_vec())
                            .map_err(|_| invalid("invalid declaration attributes"))
                    })
                    .collect::<CoreResult<Vec<_>>>()?;
                let names = keys.iter().map(Vec::as_slice).collect::<Vec<_>>();
                require(
                    matches!(
                        names.as_slice(),
                        [b"version"]
                            | [b"version", b"encoding"]
                            | [b"version", b"standalone"]
                            | [b"version", b"encoding", b"standalone"]
                    ),
                    "unsupported XML declaration attributes or order",
                )?;
                declaration_allowed = false;
            }
            Event::DocType(_) => {
                return Err(invalid("DOCTYPE and custom entities are unsupported"))
            }
            Event::Eof => break,
        }
    }
    require(
        parser.root_seen && parser.paths_seen && parser.stack.is_empty(),
        "document must close its root and contain one direct Paths child",
    )?;
    let mut counts = BTreeMap::new();
    let mut duplicates = Vec::new();
    for entry in &parser.entries {
        let count = counts.entry(entry.clone()).or_insert(0usize);
        *count += 1;
        if *count == 2 {
            duplicates.push(entry.clone());
        }
    }
    let mut warnings = Vec::new();
    if !duplicates.is_empty() {
        warnings.push("duplicate_entries".to_string());
    }
    if parser.entries.iter().any(|entry| !is_dlc_entry(entry)) {
        warnings.push("non_dlc_entries".to_string());
    }
    if parser.entries.is_empty() {
        warnings.push("empty_paths".to_string());
    }
    Ok(DlcEditorValidation {
        entries: parser.entries,
        duplicate_entries: duplicates,
        warnings,
    })
}

fn is_dlc_entry(entry: &str) -> bool {
    let prefix = r"dlcpacks:\";
    if !entry
        .get(..prefix.len())
        .is_some_and(|s| s.eq_ignore_ascii_case(prefix))
    {
        return false;
    }
    let remainder = &entry[prefix.len()..];
    let Some(pack) = remainder.strip_suffix('\\') else {
        return false;
    };
    !pack.is_empty()
        && pack != "."
        && pack != ".."
        && !pack.ends_with('.')
        && !pack.ends_with(' ')
        && !pack.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*')
        })
}

fn counts(entries: &[String]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for entry in entries {
        *counts.entry(entry.clone()).or_insert(0) += 1;
    }
    counts
}

fn difference(entries: &[String], other: &[String], retain_common: bool) -> Vec<String> {
    let mut available = counts(other);
    let mut result = Vec::new();
    for entry in entries {
        let count = available.entry(entry.clone()).or_default();
        let common = *count > 0;
        if common {
            *count -= 1;
        }
        if common == retain_common {
            result.push(entry.clone());
        }
    }
    result
}

/// Compare the ordered Item multiset; this is not a full-document text diff.
pub fn compare_dlclist(original: &str, draft: &str) -> CoreResult<DlcEditorDiff> {
    let original = validate_dlclist(original)?.entries;
    let draft = validate_dlclist(draft)?.entries;
    Ok(DlcEditorDiff {
        added: difference(&draft, &original, false),
        removed: difference(&original, &draft, false),
        reordered: difference(&original, &draft, true) != difference(&draft, &original, true),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(items: &str) -> String {
        format!("<SMandatoryPacksData><Paths>{items}</Paths></SMandatoryPacksData>")
    }

    #[test]
    fn validates_bom_crlf_comments_attributes_and_preserves_non_dlc_entries() {
        let source = "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n<!-- exported -->\r\n<SMandatoryPacksData flag=\"a&amp;b\"><Paths>\r\n<Item>dlcpacks:\\Car\\</Item><Item>platform:\\dlcPacks\\base\\</Item></Paths><Extra value=\"1\"/></SMandatoryPacksData>";
        let result = validate_dlclist(source).unwrap();
        assert_eq!(
            result.entries,
            vec![r"dlcpacks:\Car\", r"platform:\dlcPacks\base\"]
        );
        assert_eq!(result.warnings, vec!["non_dlc_entries"]);
    }

    #[test]
    fn decodes_predefined_numeric_and_cdata_text_without_reformatting() {
        let source = document(
            "<Item>dlcpacks:\\A&amp;&#66;&#x43;\\</Item><Item><![CDATA[other & literal]]></Item>",
        );
        assert_eq!(
            validate_dlclist(&source).unwrap().entries,
            vec![r"dlcpacks:\A&BC\", "other & literal"]
        );
        assert_eq!(
            compare_dlclist(&source, &source).unwrap().added,
            Vec::<String>::new()
        );
    }

    #[test]
    fn empty_paths_and_exact_duplicate_values_have_fixed_warnings() {
        assert_eq!(
            validate_dlclist(&document("")).unwrap().warnings,
            vec!["empty_paths"]
        );
        let result = validate_dlclist(&document(
            "<Item>same</Item><Item>same</Item><Item>same</Item>",
        ))
        .unwrap();
        assert_eq!(result.entries, vec!["same", "same", "same"]);
        assert_eq!(result.duplicate_entries, vec!["same"]);
        assert_eq!(
            result.warnings,
            vec!["duplicate_entries", "non_dlc_entries"]
        );
        assert_eq!(
            validate_dlclist("<SMandatoryPacksData><Paths/></SMandatoryPacksData>")
                .unwrap()
                .warnings,
            vec!["empty_paths"]
        );
    }

    #[test]
    fn suspicious_pack_paths_are_retained_with_nonstandard_warning() {
        for entry in [
            r"dlcpacks:\.\",
            r"dlcpacks:\..\",
            r"dlcpacks:\Car.\",
            r"dlcpacks:\Car \",
            r"dlcpacks:\Bad?Name\",
            "dlcpacks:\\Bad\tName\\",
            r"dlcpacks:\Bad&lt;Name\",
        ] {
            let result = validate_dlclist(&document(&format!("<Item>{entry}</Item>"))).unwrap();
            assert_eq!(result.entries.len(), 1);
            assert_eq!(result.warnings, vec!["non_dlc_entries"]);
        }
    }

    #[test]
    fn rejects_document_and_paths_structure_errors() {
        for source in [
            "", "<Other><Paths/></Other>", "<SMandatoryPacksData/>",
            "<SMandatoryPacksData><Paths/><Paths/></SMandatoryPacksData>",
            "<SMandatoryPacksData><Other><Paths/></Other></SMandatoryPacksData>",
            "<SMandatoryPacksData><Paths><Other/></Paths></SMandatoryPacksData>",
            "<SMandatoryPacksData><Paths><Item><Child/></Item></Paths></SMandatoryPacksData>",
            "<SMandatoryPacksData><Paths><Item/></Paths></SMandatoryPacksData>",
            "<SMandatoryPacksData><Paths><Item> \t\r\n</Item></Paths></SMandatoryPacksData>",
            "<SMandatoryPacksData><Paths><Item>one<!-- hidden -->two</Item></Paths></SMandatoryPacksData>",
            "<SMandatoryPacksData><Paths><Item><?hidden data?>one</Item></Paths></SMandatoryPacksData>",
            "<SMandatoryPacksData><Paths>not an item</Paths></SMandatoryPacksData>",
            "<SMandatoryPacksData><Paths></SMandatoryPacksData>",
            "<SMandatoryPacksData><Paths/>",
            "<SMandatoryPacksData><Paths/></SMandatoryPacksData><Other/>",
            "text<SMandatoryPacksData><Paths/></SMandatoryPacksData>",
        ] { assert!(validate_dlclist(source).is_err(), "accepted invalid structure"); }
    }

    #[test]
    fn rejects_doctype_custom_entities_and_invalid_xml_characters() {
        for source in [
            "<!DOCTYPE SMandatoryPacksData [<!ENTITY custom 'value'>]><SMandatoryPacksData><Paths/></SMandatoryPacksData>".to_string(),
            document("<Item>&custom;</Item>"), document("<Item>&#0;</Item>"),
            document("<Item>&#xFFFE;</Item>"), document("<Item>\u{1}</Item>"),
            document("<Item>\u{ffff}</Item>"), document("<Item>raw ]]&gt; is valid</Item>").replace("]]&gt;", "]]>"),
            "<SMandatoryPacksData extra=\"&custom;\"><Paths/></SMandatoryPacksData>".to_string(),
        ] { assert!(validate_dlclist(&source).is_err(), "accepted an unsafe XML construct"); }
    }

    #[test]
    fn rejects_invalid_names_attributes_comments_and_declarations() {
        for source in [
            "<SMandatoryPacksData><Paths/><1Bad/></SMandatoryPacksData>",
            "<SMandatoryPacksData 1bad=\"x\"><Paths/></SMandatoryPacksData>",
            "<SMandatoryPacksData a=\"x\" a=\"y\"><Paths/></SMandatoryPacksData>",
            "<SMandatoryPacksData a=\"x\"b=\"y\"><Paths/></SMandatoryPacksData>",
            "<SMandatoryPacksData a=x><Paths/></SMandatoryPacksData>",
            "<SMandatoryPacksData a=\"<\"><Paths/></SMandatoryPacksData>",
            "<!-- bad -- comment --><SMandatoryPacksData><Paths/></SMandatoryPacksData>",
            "<?xml version=\"1.0\" encoding=\"UTF-16\"?><SMandatoryPacksData><Paths/></SMandatoryPacksData>",
            "<?xml version=\"1.1\"?><SMandatoryPacksData><Paths/></SMandatoryPacksData>",
            "<?xml version=\"1.0\" extra=\"x\"?><SMandatoryPacksData><Paths/></SMandatoryPacksData>",
            "<!-- before --><?xml version=\"1.0\"?><SMandatoryPacksData><Paths/></SMandatoryPacksData>",
            "<?XML version=\"1.0\"?><SMandatoryPacksData><Paths/></SMandatoryPacksData>",
            "<SMandatoryPacksData xmlns=\"urn:unsupported\"><Paths/></SMandatoryPacksData>",
        ] { assert!(validate_dlclist(source).is_err(), "accepted invalid XML syntax"); }
        assert!(validate_dlclist(
            "<SMandatoryPacksData><Paths/><Élément attribut_字=\"ok\"/></SMandatoryPacksData>"
        )
        .is_ok());
    }

    #[test]
    fn enforces_byte_and_depth_boundaries() {
        let source = document("");
        let exact = format!("{source}{}", " ".repeat(MAX_DLCLIST_BYTES - source.len()));
        assert!(validate_dlclist(&exact).is_ok());
        assert!(validate_dlclist(&(exact + " ")).is_err());
        let deep = |count: usize| {
            format!(
                "<SMandatoryPacksData><Paths/>{}{}</SMandatoryPacksData>",
                "<Extra>".repeat(count),
                "</Extra>".repeat(count)
            )
        };
        assert!(validate_dlclist(&deep(MAX_DLCLIST_DEPTH - 1)).is_ok());
        assert!(validate_dlclist(&deep(MAX_DLCLIST_DEPTH)).is_err());
    }

    #[test]
    fn diff_preserves_duplicate_counts_and_item_order() {
        let original = document("<Item>A</Item><Item>B</Item><Item>A</Item><Item>gone</Item>");
        let draft =
            document("<Item>A</Item><Item>A</Item><Item>B</Item><Item>new</Item><Item>new</Item>");
        let diff = compare_dlclist(&original, &draft).unwrap();
        assert_eq!(diff.added, vec!["new", "new"]);
        assert_eq!(diff.removed, vec!["gone"]);
        assert!(diff.reordered);
        let only_added = compare_dlclist(
            &document("<Item>A</Item><Item>B</Item>"),
            &document("<Item>A</Item><Item>new</Item><Item>B</Item>"),
        )
        .unwrap();
        assert!(!only_added.reordered);
        assert_eq!(
            compare_dlclist(
                &document("<Item>A</Item><Item>A</Item>"),
                &document("<Item>A</Item>")
            )
            .unwrap()
            .removed,
            vec!["A"]
        );
        assert!(compare_dlclist(&original, "invalid").is_err());
    }
}

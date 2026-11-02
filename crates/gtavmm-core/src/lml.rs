// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
// OpenAI-Codex_lml_parser_impl
// OpenAI-Codex

//! Declaration-only EasyInstall manifest parsing. No filesystem access, resource
//! resolution, installation, or complete conflict analysis is performed here.

use quick_xml::{
    events::{BytesStart, Event},
    Reader,
};
use std::collections::BTreeMap;

pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
pub const MAX_DEPTH: usize = 64;
pub const MAX_EVENTS: usize = 10_000;
pub const MAX_PATH_BYTES: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalReference(String);

impl LocalReference {
    fn parse(value: &str) -> Result<Self, String> {
        require(
            !value.is_empty() && value.len() <= MAX_PATH_BYTES,
            "invalid local path length",
        )?;
        require(
            !value.chars().any(|c| {
                c.is_control() || matches!(c, '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*')
            }),
            "unsupported local path character",
        )?;
        for part in value.split('/') {
            require(
                !part.is_empty() && part != "." && part != ".." && !part.ends_with(['.', ' ']),
                "unsafe local path component",
            )?;
            let stem = part
                .split('.')
                .next()
                .unwrap_or_default()
                .trim_end_matches(' ')
                .to_ascii_uppercase();
            let numbered_device = ["COM", "LPT"].iter().any(|prefix| {
                stem.strip_prefix(prefix).is_some_and(|n| {
                    matches!(
                        n,
                        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                    )
                })
            });
            require(
                !matches!(
                    stem.as_str(),
                    "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
                ) && !numbered_device,
                "reserved Windows path component",
            )?;
        }
        Ok(Self(value.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An opaque virtual game declaration, never a host filesystem path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GamePath(String);

impl GamePath {
    fn parse(value: &str) -> Result<Self, String> {
        require(
            !value.is_empty()
                && value.len() <= MAX_PATH_BYTES
                && !value.chars().any(char::is_control),
            "invalid virtual game path",
        )?;
        Ok(Self(value.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceKind {
    Streaming,
    Data,
    Replacement,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceShape {
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceDeclaration {
    pub kind: ResourceKind,
    pub shape: ReferenceShape,
    pub local_path: LocalReference,
    pub game_path: Option<GamePath>,
    pub data_type: Option<String>,
    pub group_name: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManifestMetadata {
    pub name: Option<String>,
    pub author: Option<String>,
    pub version: Option<String>,
    pub link: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LmlManifest {
    pub metadata: ManifestMetadata,
    pub resources: Vec<ResourceDeclaration>,
}

fn require(ok: bool, reason: &str) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(format!("LML manifest: {reason}"))
    }
}
fn xml_char(c: char) -> bool {
    matches!(c as u32, 9 | 10 | 13 | 0x20..=0xd7ff | 0xe000..=0xfffd | 0x10000..=0x10ffff)
}
fn space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n')
}
fn decode(bytes: &[u8]) -> Result<String, String> {
    let raw = std::str::from_utf8(bytes).map_err(|_| "LML manifest: invalid UTF-8".to_owned())?;
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    let value = quick_xml::escape::unescape(&normalized)
        .map_err(|_| "LML manifest: invalid entity".to_owned())?;
    require(
        value.chars().all(xml_char),
        "invalid XML character reference",
    )?;
    Ok(value.into_owned())
}

#[derive(Debug)]
struct Node {
    name: String,
    attrs: BTreeMap<String, String>,
    text: String,
    children: Vec<Node>,
}

fn node(tag: &BytesStart<'_>) -> Result<Node, String> {
    let name = std::str::from_utf8(tag.name().as_ref())
        .map_err(|_| "invalid tag name".to_owned())?
        .to_owned();
    require(
        !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric()),
        "unsupported tag name or namespace",
    )?;
    // Enforce XML whitespace between attributes, including adjacent quoted values.
    let bytes = tag.as_ref();
    let mut offset = name.len();
    while offset < bytes.len() {
        require(
            matches!(bytes[offset], b' ' | b'\t' | b'\r' | b'\n'),
            "missing attribute separator",
        )?;
        while offset < bytes.len() && matches!(bytes[offset], b' ' | b'\t' | b'\r' | b'\n') {
            offset += 1;
        }
        if offset == bytes.len() {
            break;
        }
        offset += bytes[offset..]
            .iter()
            .position(|b| *b == b'=')
            .ok_or("attribute lacks equals")?
            + 1;
        while offset < bytes.len() && matches!(bytes[offset], b' ' | b'\t' | b'\r' | b'\n') {
            offset += 1;
        }
        let quote = *bytes.get(offset).ok_or("attribute lacks value")?;
        require(matches!(quote, b'\'' | b'"'), "unquoted attribute")?;
        offset += 1;
        offset += bytes[offset..]
            .iter()
            .position(|b| *b == quote)
            .ok_or("unterminated attribute")?
            + 1;
    }
    let mut attrs = BTreeMap::new();
    for attr in tag.attributes().with_checks(true) {
        let attr = attr.map_err(|_| "invalid or duplicate attribute".to_owned())?;
        let key = std::str::from_utf8(attr.key.as_ref())
            .map_err(|_| "invalid attribute name".to_owned())?;
        require(
            (matches!(name.as_str(), "Addon" | "Replacement") && key == "name")
                || (name == "DataFile" && key == "type")
                || (name == "xml" && matches!(key, "version" | "encoding" | "standalone")),
            "unsupported attribute",
        )?;
        require(
            !attr.value.contains(&b'<'),
            "literal less-than in attribute",
        )?;
        let raw = std::str::from_utf8(attr.value.as_ref())
            .map_err(|_| "invalid attribute UTF-8".to_owned())?;
        // XML normalizes literal attribute whitespace before resolving references.
        let normalized = raw
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .replace(['\t', '\n'], " ");
        attrs.insert(key.to_owned(), decode(normalized.as_bytes())?);
    }
    Ok(Node {
        name,
        attrs,
        text: String::new(),
        children: Vec::new(),
    })
}

/// Parse a bounded, deliberately closed subset of EasyInstall declarations.
/// Successful parsing does not establish loader compatibility or installability.
pub fn parse_manifest(bytes: &[u8]) -> Result<LmlManifest, String> {
    require(bytes.len() <= MAX_MANIFEST_BYTES, "manifest exceeds 1 MiB")?;
    let content =
        std::str::from_utf8(bytes).map_err(|_| "LML manifest: invalid UTF-8".to_owned())?;
    require(content.chars().all(xml_char), "invalid XML character")?;
    let mut reader = Reader::from_str(content.strip_prefix('\u{feff}').unwrap_or(content));
    let mut stack: Vec<Node> = Vec::new();
    let mut root = None;
    let mut declaration_allowed = true;
    for _ in 0..MAX_EVENTS {
        let event = reader
            .read_event()
            .map_err(|_| "LML manifest: malformed XML".to_owned())?;
        match event {
            Event::Start(tag) => {
                require(stack.len() < MAX_DEPTH, "depth exceeds 64")?;
                require(!stack.is_empty() || root.is_none(), "multiple roots")?;
                stack.push(node(&tag)?);
            }
            Event::Empty(tag) => {
                require(stack.len() < MAX_DEPTH, "depth exceeds 64")?;
                attach(node(&tag)?, &mut stack, &mut root)?;
            }
            Event::End(tag) => {
                let finished = stack.pop().ok_or("unexpected closing tag")?;
                require(
                    finished.name.as_bytes() == tag.name().as_ref(),
                    "mismatched closing tag",
                )?;
                attach(finished, &mut stack, &mut root)?;
            }
            Event::Text(text) => {
                require(
                    !text.as_ref().windows(3).any(|b| b == b"]]>"),
                    "literal CDATA terminator",
                )?;
                if let Some(current) = stack.last_mut() {
                    current.text.push_str(&decode(text.as_ref())?);
                } else {
                    require(
                        text.as_ref()
                            .iter()
                            .all(|b| matches!(*b, b' ' | b'\t' | b'\r' | b'\n')),
                        "text outside root",
                    )?;
                }
            }
            Event::CData(text) => {
                let current = stack.last_mut().ok_or("CDATA outside root")?;
                let value =
                    std::str::from_utf8(text.as_ref()).map_err(|_| "invalid CDATA".to_owned())?;
                current
                    .text
                    .push_str(&value.replace("\r\n", "\n").replace('\r', "\n"));
            }
            Event::Decl(decl) => {
                require(declaration_allowed, "misplaced declaration")?;
                let raw = std::str::from_utf8(decl.as_ref())
                    .map_err(|_| "invalid declaration".to_owned())?;
                require(
                    !raw.contains('&'),
                    "references are forbidden in XML declarations",
                )?;
                let declaration = node(&BytesStart::from_content(raw, 3))?;
                let keys: Vec<_> = BytesStart::from_content(raw, 3)
                    .attributes()
                    .map(|a| {
                        a.map(|a| a.key.as_ref().to_vec())
                            .map_err(|_| "invalid declaration attributes")
                    })
                    .collect::<Result<_, _>>()?;
                let names: Vec<_> = keys.iter().map(Vec::as_slice).collect();
                require(
                    matches!(
                        names.as_slice(),
                        [b"version"]
                            | [b"version", b"encoding"]
                            | [b"version", b"standalone"]
                            | [b"version", b"encoding", b"standalone"]
                    ),
                    "invalid declaration attribute order",
                )?;
                require(
                    declaration.attrs.get("version").is_some_and(|v| v == "1.0"),
                    "only XML 1.0 is supported",
                )?;
                require(
                    declaration
                        .attrs
                        .get("encoding")
                        .is_none_or(|v| v.eq_ignore_ascii_case("UTF-8")),
                    "only UTF-8 is supported",
                )?;
                require(
                    declaration
                        .attrs
                        .get("standalone")
                        .is_none_or(|v| v == "yes" || v == "no"),
                    "invalid standalone",
                )?;
            }
            Event::Eof => {
                require(stack.is_empty(), "unclosed element")?;
                return interpret(root.ok_or("missing EasyInstall root")?);
            }
            Event::Comment(comment) => {
                let value = comment.as_ref();
                require(
                    !value.windows(2).any(|b| b == b"--") && !value.ends_with(b"-"),
                    "malformed XML comment",
                )?;
            }
            Event::PI(_) | Event::DocType(_) => {
                return Err(
                    "LML manifest: processing instructions and DTDs are unsupported".to_owned(),
                )
            }
        }
        declaration_allowed = false;
    }
    Err("LML manifest: event limit exceeded".to_owned())
}

fn attach(value: Node, stack: &mut [Node], root: &mut Option<Node>) -> Result<(), String> {
    if let Some(parent) = stack.last_mut() {
        parent.children.push(value);
    } else {
        require(root.is_none(), "multiple roots")?;
        *root = Some(value);
    }
    Ok(())
}
fn container(node: &Node) -> Result<(), String> {
    require(node.text.chars().all(space), "mixed container text")
}
fn leaf(node: &Node) -> Result<&str, String> {
    require(node.children.is_empty(), "unexpected nested element")?;
    Ok(&node.text)
}
fn unique_child<'a>(node: &'a Node, name: &str) -> Result<&'a Node, String> {
    let mut found = node.children.iter().filter(|n| n.name == name);
    let value = found.next().ok_or_else(|| format!("missing {name}"))?;
    require(found.next().is_none(), "duplicate path field")?;
    Ok(value)
}
fn interpret(root: Node) -> Result<LmlManifest, String> {
    require(root.name == "EasyInstall", "expected EasyInstall root")?;
    container(&root)?;
    let mut manifest = LmlManifest::default();
    let mut seen = std::collections::BTreeSet::new();
    for child in &root.children {
        require(
            seen.insert(child.name.as_str()),
            "duplicate top-level element",
        )?;
        let slot = match child.name.as_str() {
            "Name" => Some(&mut manifest.metadata.name),
            "Author" => Some(&mut manifest.metadata.author),
            "Version" => Some(&mut manifest.metadata.version),
            "Link" => Some(&mut manifest.metadata.link),
            "Description" => Some(&mut manifest.metadata.description),
            _ => None,
        };
        if let Some(slot) = slot {
            *slot = Some(leaf(child)?.to_owned());
            continue;
        }
        let group_tag = match child.name.as_str() {
            "Addons" => "Addon",
            "Replacements" => "Replacement",
            _ => return Err(format!("unsupported element {}", child.name)),
        };
        container(child)?;
        for group in &child.children {
            require(group.name == group_tag, "unsupported group element")?;
            container(group)?;
            for resource in &group.children {
                let (kind, shape) = match (group_tag, resource.name.as_str()) {
                    ("Addon", "StreamingFile") => (ResourceKind::Streaming, ReferenceShape::File),
                    ("Addon", "StreamingFiles") => {
                        (ResourceKind::Streaming, ReferenceShape::Directory)
                    }
                    ("Addon", "DataFile") => (ResourceKind::Data, ReferenceShape::File),
                    ("Addon", "DataFiles") => (ResourceKind::Data, ReferenceShape::Directory),
                    ("Replacement", "FileReplacement") => {
                        (ResourceKind::Replacement, ReferenceShape::File)
                    }
                    ("Replacement", "FileReplacements") => {
                        (ResourceKind::Replacement, ReferenceShape::Directory)
                    }
                    _ => return Err(format!("unsupported resource {}", resource.name)),
                };
                let (local, game_path) =
                    if kind == ResourceKind::Replacement && !resource.children.is_empty() {
                        container(resource)?;
                        require(
                            resource.children.len() == 2,
                            "replacement requires exactly GamePath and FilePath",
                        )?;
                        (
                            leaf(unique_child(resource, "FilePath")?)?,
                            Some(GamePath::parse(leaf(unique_child(resource, "GamePath")?)?)?),
                        )
                    } else {
                        (leaf(resource)?, None)
                    };
                let data_type = resource.attrs.get("type").cloned();
                require(
                    data_type.as_ref().is_none_or(|v| !v.trim().is_empty()),
                    "empty data type",
                )?;
                manifest.resources.push(ResourceDeclaration {
                    kind,
                    shape,
                    local_path: LocalReference::parse(local)?,
                    game_path,
                    data_type,
                    group_name: group.attrs.get("name").cloned(),
                });
            }
        }
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn representative_declarations_remain_distinct() {
        let manifest = parse_manifest(br#"<?xml version="1.0" encoding="UTF-8"?>
<EasyInstall><Name>A &amp; B</Name><Author>Author</Author><Version>1</Version>
<Link>https://example.invalid</Link><Description>Example</Description>
<Addons><Addon name="Example"><StreamingFile>stream/a.ytd</StreamingFile>
<StreamingFiles>stream</StreamingFiles><DataFile type="TEST">data/a.meta</DataFile>
<DataFiles>data</DataFiles></Addon></Addons><Replacements><Replacement name="R">
<FileReplacement><GamePath>update:/common/data/a.meta</GamePath><FilePath>files/a.meta</FilePath></FileReplacement>
<FileReplacements>replace</FileReplacements></Replacement></Replacements></EasyInstall>"#).unwrap();
        assert_eq!(manifest.metadata.name.as_deref(), Some("A & B"));
        assert_eq!(manifest.resources.len(), 6);
        assert_eq!(manifest.resources[1].shape, ReferenceShape::Directory);
        assert_eq!(manifest.resources[2].data_type.as_deref(), Some("TEST"));
        assert_eq!(
            manifest.resources[4].game_path.as_ref().unwrap().as_str(),
            "update:/common/data/a.meta"
        );
    }

    #[test]
    fn rejects_unsupported_and_duplicate_semantics() {
        for body in ["<Name>A</Name><Name>B</Name>", "<Resources/>", "<ResourceLua/>",
            "<DlcRpf/>", "<IsExtension/>", "<ResourceFolders/>", "<Addons/><Addons/>",
            "<Addons><Addon allowDuplicate=\"true\"/></Addons>",
            "<Addons><Addon><DataFiles type=\"X\">data</DataFiles></Addon></Addons>",
            "<Replacements><Replacement><FileReplacement><FilePath>a</FilePath></FileReplacement></Replacement></Replacements>"] {
            assert!(parse_manifest(format!("<EasyInstall>{body}</EasyInstall>").as_bytes()).is_err(), "{body}");
        }
    }

    #[test]
    fn rejects_unsafe_local_paths() {
        for path in [
            "",
            "/a",
            "C:/a",
            "../a",
            "a/../b",
            "a//b",
            "a/./b",
            "a\\..\\b",
            "a:b",
            "//server/a",
            "CON",
            "aux.txt",
            "LPT9.bin",
            "a.",
            "a ",
            "a/?b",
        ] {
            assert!(LocalReference::parse(path).is_err(), "{path}");
        }
        assert_eq!(
            LocalReference::parse("dir/file.ytd").unwrap().as_str(),
            "dir/file.ytd"
        );
    }

    #[test]
    fn rejects_malformed_or_unsupported_xml() {
        for xml in [
            "<EasyInstall/><EasyInstall/>",
            "<EasyInstall>",
            "<EasyInstall>&custom;</EasyInstall>",
            "<!DOCTYPE EasyInstall><EasyInstall/>",
            "<?go x?><EasyInstall/>",
            "<EasyInstall><!--bad--comment--></EasyInstall>",
            "<EasyInstall xmlns=\"x\"/>",
            "<EasyInstall><Name>&#0;</Name></EasyInstall>",
            "<EasyInstall>\u{1}</EasyInstall>",
            "<?xml version=\"1.1\"?><EasyInstall/>",
            "<?xml version=\"1.0\" encoding=\"UTF-16\"?><EasyInstall/>",
            "<EasyInstall><Name a=\"1\" a=\"2\"/></EasyInstall>",
            "<EasyInstall><Name>abc]]></Name></EasyInstall>",
        ] {
            assert!(parse_manifest(xml.as_bytes()).is_err(), "{xml}");
        }
        assert!(parse_manifest(&[0xff]).is_err());
        assert_eq!(
            parse_manifest(b"<EasyInstall><Name>&#65;&lt;</Name></EasyInstall>")
                .unwrap()
                .metadata
                .name
                .as_deref(),
            Some("A<")
        );
    }

    #[test]
    fn accepts_legal_comments_and_rejects_malformed_comments() {
        assert!(parse_manifest(b"<!--before--><EasyInstall><!--inside--><Name>A<!--split-->B</Name></EasyInstall><!--after-->").is_ok());
        for xml in [
            "<EasyInstall><!--a--b--></EasyInstall>",
            "<EasyInstall><!--a---></EasyInstall>",
        ] {
            assert!(parse_manifest(xml.as_bytes()).is_err());
        }
    }

    #[test]
    fn rejects_reserved_stems_with_spaces_before_extension() {
        for path in ["CON .txt", "AUX .meta", "COM1 .bin", "dir/LPT2  .dat"] {
            assert!(LocalReference::parse(path).is_err(), "{path}");
        }
        assert_eq!(
            LocalReference::parse("ordinary .txt").unwrap().as_str(),
            "ordinary .txt"
        );
    }

    #[test]
    fn normalizes_literal_attribute_whitespace_before_entity_decoding() {
        let xml = b"<EasyInstall><Name>A\tB\nC</Name><Addons><Addon name=\"A\tB\r\nC\rD&#9;E&#10;F\"><DataFile type=\"X\tY\nZ&#9;W\">a.meta</DataFile></Addon></Addons></EasyInstall>";
        let manifest = parse_manifest(xml).unwrap();
        assert_eq!(manifest.metadata.name.as_deref(), Some("A\tB\nC"));
        assert_eq!(
            manifest.resources[0].group_name.as_deref(),
            Some("A B C D\tE\nF")
        );
        assert_eq!(manifest.resources[0].data_type.as_deref(), Some("X Y Z\tW"));
    }

    #[test]
    fn enforces_bounds() {
        assert!(parse_manifest(&vec![b' '; MAX_MANIFEST_BYTES + 1]).is_err());
        assert!(LocalReference::parse(&"a".repeat(MAX_PATH_BYTES + 1)).is_err());
        let deep = format!(
            "{}{}",
            "<EasyInstall>".repeat(MAX_DEPTH + 1),
            "</EasyInstall>".repeat(MAX_DEPTH + 1)
        );
        assert!(parse_manifest(deep.as_bytes()).is_err());
        let many = format!(
            "<EasyInstall><Addons>{}</Addons></EasyInstall>",
            "<Addon/>".repeat(MAX_EVENTS)
        );
        assert!(parse_manifest(many.as_bytes()).is_err());
    }
}

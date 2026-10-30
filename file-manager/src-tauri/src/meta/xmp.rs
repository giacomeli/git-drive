//! XMP packets: the descriptive fields read from one, and an edit merged
//! into one. The packet is kept as a tree of what it holds, raw (escaped
//! text and attribute values as written), so whatever this app does not
//! manage (a raw developer's settings, a camera's own namespaces) is written
//! back as it was; only the managed properties are removed (in either form:
//! an attribute of `rdf:Description`, or an element) and written anew.

use quick_xml::escape::{escape, unescape};
use quick_xml::events::Event;
use quick_xml::Reader;

pub const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
pub const DC: &str = "http://purl.org/dc/elements/1.1/";
pub const XMP: &str = "http://ns.adobe.com/xap/1.0/";
pub const PHOTOSHOP: &str = "http://ns.adobe.com/photoshop/1.0/";
pub const EXIF: &str = "http://ns.adobe.com/exif/1.0/";
const X: &str = "adobe:ns:meta/";
const XML: &str = "http://www.w3.org/XML/1998/namespace";

/// The prefix each namespace gets when this app declares it.
const PREFIXES: [(&str, &str); 6] = [(RDF, "rdf"), (DC, "dc"), (XMP, "xmp"), (PHOTOSHOP, "photoshop"), (EXIF, "exif"), (X, "x")];

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Elem(Elem),
    /// Text as written (escaped).
    Text(String),
    /// A comment, a CDATA section or a processing instruction, as written.
    Raw(String),
}

#[derive(Debug, Clone, PartialEq)]
struct Elem {
    name: String,
    /// Names and values as written (values escaped).
    attrs: Vec<(String, String)>,
    children: Vec<Node>,
}

impl Elem {
    fn new(name: &str) -> Elem {
        Elem { name: name.to_string(), attrs: Vec::new(), children: Vec::new() }
    }
    fn attr(mut self, k: &str, v: &str) -> Elem {
        self.attrs.push((k.to_string(), escape(v).into_owned()));
        self
    }
    fn child(mut self, c: Node) -> Elem {
        self.children.push(c);
        self
    }
}

/// The prefixes in scope: (prefix, namespace), innermost last.
type Scope = Vec<(String, String)>;

fn split(qname: &str) -> (&str, &str) {
    qname.split_once(':').unwrap_or(("", qname))
}

fn declared(e: &Elem) -> impl Iterator<Item = (String, String)> + '_ {
    e.attrs.iter().filter_map(|(k, v)| {
        let uri = unescape(v).map(|u| u.into_owned()).unwrap_or_else(|_| v.clone());
        if k == "xmlns" {
            Some((String::new(), uri))
        } else {
            k.strip_prefix("xmlns:").map(|p| (p.to_string(), uri))
        }
    })
}

fn resolve<'a>(scope: &'a Scope, prefix: &str) -> Option<&'a str> {
    if prefix == "xml" {
        return Some(XML);
    }
    scope.iter().rev().find(|(p, _)| p == prefix).map(|(_, u)| u.as_str())
}

/// (namespace, local name) of a qualified name in `scope`.
fn expanded<'a>(scope: &'a Scope, qname: &'a str) -> (Option<&'a str>, &'a str) {
    let (prefix, local) = split(qname);
    (resolve(scope, prefix), local)
}

fn parse(text: &str) -> Option<Vec<Node>> {
    let mut reader = Reader::from_str(text);
    reader.config_mut().trim_text(false);
    let mut stack: Vec<Elem> = Vec::new();
    let mut top: Vec<Node> = Vec::new();
    let push = |stack: &mut Vec<Elem>, top: &mut Vec<Node>, n: Node| match stack.last_mut() {
        Some(parent) => parent.children.push(n),
        None => top.push(n),
    };
    let elem = |e: &quick_xml::events::BytesStart| -> Option<Elem> {
        let name = e.name().0.to_string();
        let mut attrs = Vec::new();
        for a in e.attributes().with_checks(false) {
            let a = a.ok()?;
            attrs.push((a.key.0.to_string(), a.value.into_owned()));
        }
        Some(Elem { name, attrs, children: Vec::new() })
    };
    loop {
        match reader.read_event().ok()? {
            Event::Start(e) => stack.push(elem(&e)?),
            Event::Empty(e) => {
                let el = elem(&e)?;
                push(&mut stack, &mut top, Node::Elem(el));
            }
            Event::End(_) => {
                let done = stack.pop()?;
                push(&mut stack, &mut top, Node::Elem(done));
            }
            Event::Text(t) => push(&mut stack, &mut top, Node::Text(t.into_inner().into_owned())),
            Event::GeneralRef(r) => push(&mut stack, &mut top, Node::Text(format!("&{};", r.into_inner()))),
            Event::CData(c) => push(&mut stack, &mut top, Node::Raw(format!("<![CDATA[{}]]>", c.into_inner()))),
            Event::Comment(c) => push(&mut stack, &mut top, Node::Raw(format!("<!--{}-->", c.into_inner()))),
            Event::PI(p) => push(&mut stack, &mut top, Node::Raw(format!("<?{}?>", p.into_inner()))),
            Event::Decl(_) | Event::DocType(_) => {}
            Event::Eof => break,
        }
    }
    stack.is_empty().then_some(top)
}

fn write_node(n: &Node, out: &mut String) {
    match n {
        Node::Text(t) | Node::Raw(t) => out.push_str(t),
        Node::Elem(e) => {
            out.push('<');
            out.push_str(&e.name);
            for (k, v) in &e.attrs {
                let quote = if v.contains('"') { '\'' } else { '"' };
                out.push(' ');
                out.push_str(k);
                out.push('=');
                out.push(quote);
                out.push_str(v);
                out.push(quote);
            }
            if e.children.is_empty() {
                out.push_str("/>");
            } else {
                out.push('>');
                for c in &e.children {
                    write_node(c, out);
                }
                out.push_str("</");
                out.push_str(&e.name);
                out.push('>');
            }
        }
    }
}

fn text_of(e: &Elem) -> String {
    let raw: String = e.children.iter().filter_map(|c| if let Node::Text(t) = c { Some(t.as_str()) } else { None }).collect();
    unescape(&raw).map(|s| s.into_owned()).unwrap_or(raw).trim().to_string()
}

/// The descriptive fields of a packet (the first value found of each).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Fields {
    pub title: Option<String>,
    pub description: Option<String>,
    pub creator: Vec<String>,
    pub rights: Option<String>,
    pub subject: Vec<String>,
    pub rating: Option<i32>,
    /// photoshop:DateCreated, else exif:DateTimeOriginal (ISO 8601).
    pub date_created: Option<String>,
    /// exif:GPS* properties are there.
    pub has_gps: bool,
}

/// The items of a property's array (`rdf:Alt`, `rdf:Bag`, `rdf:Seq`), or its own text.
fn values(e: &Elem, scope: &Scope) -> Vec<String> {
    for c in &e.children {
        if let Node::Elem(arr) = c {
            if expanded(scope, &arr.name).0 == Some(RDF) {
                let items: Vec<&Elem> = arr.children.iter().filter_map(|n| if let Node::Elem(li) = n { Some(li) } else { None }).collect();
                // An Alt's default language first.
                let default = items.iter().position(|li| li.attrs.iter().any(|(k, v)| k == "xml:lang" && v == "x-default"));
                let mut out: Vec<String> = items.iter().map(|li| text_of(li)).collect();
                if let Some(i) = default {
                    out.swap(0, i);
                }
                return out.into_iter().filter(|s| !s.is_empty()).collect();
            }
        }
    }
    let t = text_of(e);
    if t.is_empty() {
        Vec::new()
    } else {
        vec![t]
    }
}

fn read_into(nodes: &[Node], scope: &mut Scope, f: &mut Fields) {
    for n in nodes {
        let Node::Elem(e) = n else { continue };
        let depth = scope.len();
        scope.extend(declared(e));
        if expanded(scope, &e.name) == (Some(RDF), "Description") {
            let mut found: Vec<(String, String, Vec<String>)> = Vec::new();
            for (k, v) in &e.attrs {
                if let (Some(ns), local) = expanded(scope, k) {
                    let value = unescape(v).map(|s| s.into_owned()).unwrap_or_else(|_| v.clone());
                    found.push((ns.to_string(), local.to_string(), vec![value]));
                }
            }
            for c in &e.children {
                if let Node::Elem(p) = c {
                    let inner = scope.len();
                    scope.extend(declared(p));
                    if let (Some(ns), local) = expanded(scope, &p.name) {
                        found.push((ns.to_string(), local.to_string(), values(p, scope)));
                    }
                    scope.truncate(inner);
                }
            }
            for (ns, local, vals) in found {
                let first = vals.first().cloned();
                match (ns.as_str(), local.as_str()) {
                    (DC, "title") if f.title.is_none() => f.title = first,
                    (DC, "description") if f.description.is_none() => f.description = first,
                    (DC, "rights") if f.rights.is_none() => f.rights = first,
                    (DC, "creator") if f.creator.is_empty() => f.creator = vals,
                    (DC, "subject") if f.subject.is_empty() => f.subject = vals,
                    (XMP, "Rating") if f.rating.is_none() => f.rating = first.and_then(|r| r.trim().parse::<f64>().ok()).map(|r| r.round() as i32),
                    (PHOTOSHOP, "DateCreated") => f.date_created = first.or(f.date_created.take()),
                    (EXIF, "DateTimeOriginal") if f.date_created.is_none() => f.date_created = first,
                    (EXIF, l) if l.starts_with("GPS") => f.has_gps = true,
                    _ => {}
                }
            }
        }
        read_into(&e.children, scope, f);
        scope.truncate(depth);
    }
}

/// The descriptive fields of `packet`; empty when it does not parse.
pub fn read(packet: &str) -> Fields {
    let mut f = Fields::default();
    if let Some(nodes) = parse(packet) {
        read_into(&nodes, &mut Vec::new(), &mut f);
    }
    f
}

/// A property's new value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Simple(String),
    /// A language alternative (title, description, rights): the default language.
    Alt(String),
    Bag(Vec<String>),
    Seq(Vec<String>),
}

/// What an edit does to a packet: properties set (`Some`) or removed
/// (`None`), and families of properties removed by their name's start.
#[derive(Debug, Clone, Default)]
pub struct Edit {
    pub props: Vec<(&'static str, &'static str, Option<Value>)>,
    pub remove_starting: Vec<(&'static str, &'static str)>,
}

impl Edit {
    fn touches(&self, ns: Option<&str>, local: &str) -> bool {
        let Some(ns) = ns else { return false };
        self.props.iter().any(|(n, l, _)| *n == ns && *l == local) || self.remove_starting.iter().any(|(n, p)| *n == ns && local.starts_with(p))
    }
    pub fn is_empty(&self) -> bool {
        self.props.is_empty() && self.remove_starting.is_empty()
    }
}

/// Removes what `edit` manages from every `rdf:Description`; returns whether one was seen.
fn strip(nodes: &mut [Node], scope: &mut Scope, edit: &Edit) {
    for n in nodes.iter_mut() {
        let Node::Elem(e) = n else { continue };
        let depth = scope.len();
        scope.extend(declared(e));
        if expanded(scope, &e.name) == (Some(RDF), "Description") {
            let keep: Vec<(String, String)> = e.attrs.iter().filter(|(k, _)| { let (ns, local) = expanded(scope, k); !edit.touches(ns, local) }).cloned().collect();
            e.attrs = keep;
            let sc = scope.clone();
            e.children.retain(|c| match c {
                Node::Elem(p) => {
                    let mut s = sc.clone();
                    s.extend(declared(p));
                    let (ns, local) = expanded(&s, &p.name);
                    !edit.touches(ns, local)
                }
                _ => true,
            });
        }
        strip(&mut e.children, scope, edit);
        scope.truncate(depth);
    }
}

/// The path (indices) of the first `rdf:Description` and the scope at it.
fn first_description(nodes: &[Node], scope: &mut Scope, path: &mut Vec<usize>) -> Option<Scope> {
    for (i, n) in nodes.iter().enumerate() {
        let Node::Elem(e) = n else { continue };
        let depth = scope.len();
        scope.extend(declared(e));
        path.push(i);
        if expanded(scope, &e.name) == (Some(RDF), "Description") {
            return Some(scope.clone());
        }
        if let Some(s) = first_description(&e.children, scope, path) {
            return Some(s);
        }
        path.pop();
        scope.truncate(depth);
    }
    None
}

fn at_path<'a>(nodes: &'a mut [Node], path: &[usize]) -> Option<&'a mut Elem> {
    let (first, rest) = path.split_first()?;
    let Node::Elem(e) = nodes.get_mut(*first)? else { return None };
    if rest.is_empty() {
        Some(e)
    } else {
        at_path(&mut e.children, rest)
    }
}

/// The root element of a new packet: `x:xmpmeta` > `rdf:RDF` > one empty `rdf:Description`.
fn fresh() -> Vec<Node> {
    let desc = Elem::new("rdf:Description").attr("rdf:about", "");
    let rdf = Elem::new("rdf:RDF").attr("xmlns:rdf", RDF).child(Node::Elem(desc));
    vec![Node::Elem(Elem::new("x:xmpmeta").attr("xmlns:x", X).attr("x:xmptk", "git-drive Files").child(Node::Elem(rdf)))]
}

/// The packet with `edit` merged: what it does not manage stays as it was.
/// A packet that does not parse (or none) is replaced by a fresh one.
pub fn merge(packet: Option<&str>, edit: &Edit) -> String {
    let mut nodes = packet.and_then(parse).filter(|n| first_description(n, &mut Vec::new(), &mut Vec::new()).is_some()).unwrap_or_else(fresh);
    // Only the root element: the old packet's wrapper is written anew.
    nodes.retain(|n| matches!(n, Node::Elem(_)));
    strip(&mut nodes, &mut Vec::new(), edit);
    let mut path = Vec::new();
    let scope = first_description(&nodes, &mut Vec::new(), &mut path).unwrap_or_default();
    let target = at_path(&mut nodes, &path).expect("a description was found");
    // A prefix for each namespace the new values use: one in scope, else declared here.
    let prefix_of = |ns: &str, target: &mut Elem| -> String {
        if let Some((p, _)) = scope.iter().rev().find(|(p, u)| u == ns && !p.is_empty()) {
            return p.clone();
        }
        if let Some((p, _)) = declared(target).find(|(p, u)| u == ns && !p.is_empty()) {
            return p;
        }
        let base = PREFIXES.iter().find(|(u, _)| *u == ns).map_or("ns", |(_, p)| p);
        let taken = |p: &str| scope.iter().any(|(q, _)| q == p) || target.attrs.iter().any(|(k, _)| k == &format!("xmlns:{p}"));
        let prefix = std::iter::once(base.to_string()).chain((1..).map(|i| format!("{base}{i}"))).find(|p| !taken(p)).expect("a free prefix");
        target.attrs.push((format!("xmlns:{prefix}"), ns.to_string()));
        prefix
    };
    let rdf = prefix_of(RDF, target);
    for (ns, local, value) in &edit.props {
        let Some(value) = value else { continue };
        let p = prefix_of(ns, target);
        let li = |t: &str| Node::Elem(Elem::new(&format!("{rdf}:li")).child(Node::Text(escape(t).into_owned())));
        let prop = Elem::new(&format!("{p}:{local}"));
        let prop = match value {
            Value::Simple(v) => prop.child(Node::Text(escape(v).into_owned())),
            Value::Alt(v) => prop.child(Node::Elem(
                Elem::new(&format!("{rdf}:Alt")).child(Node::Elem(Elem::new(&format!("{rdf}:li")).attr("xml:lang", "x-default").child(Node::Text(escape(v).into_owned())))),
            )),
            Value::Bag(vs) => prop.child(Node::Elem(vs.iter().fold(Elem::new(&format!("{rdf}:Bag")), |a, v| a.child(li(v))))),
            Value::Seq(vs) => prop.child(Node::Elem(vs.iter().fold(Elem::new(&format!("{rdf}:Seq")), |a, v| a.child(li(v))))),
        };
        target.children.push(Node::Elem(prop));
    }
    let mut out = String::from("<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n");
    for n in &nodes {
        write_node(n, &mut out);
    }
    // Room for a later edit in place, as XMP writers leave.
    out.push('\n');
    for _ in 0..20 {
        out.push_str(&" ".repeat(99));
        out.push('\n');
    }
    out.push_str("<?xpacket end=\"w\"?>");
    out
}

/// The XMP packet inside `bytes`, wherever it is (a format-blind scan for
/// `<x:xmpmeta ...>...</x:xmpmeta>`, as readers do for files they do not parse).
pub fn find_packet(bytes: &[u8]) -> Option<String> {
    let start = memfind(bytes, b"<x:xmpmeta")?;
    let end = start + memfind(&bytes[start..], b"</x:xmpmeta>")? + b"</x:xmpmeta>".len();
    String::from_utf8(bytes[start..end].to_vec()).ok()
}

// Used when a HEIC is stripped through ImageIO (macOS); the tests use it everywhere.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
/// Empties the XMP packet inside `bytes` where it lies: its `x:xmpmeta`
/// becomes an empty one padded with spaces to the same length, so nothing
/// moves (a HEIC's item offsets stay right). Returns whether there was one.
pub fn blank_in_place(bytes: &mut [u8]) -> bool {
    let Some(start) = memfind(bytes, b"<x:xmpmeta") else { return false };
    let Some(len) = memfind(&bytes[start..], b"</x:xmpmeta>").map(|e| e + b"</x:xmpmeta>".len()) else { return false };
    let empty = br#"<x:xmpmeta xmlns:x="adobe:ns:meta/"/>"#;
    if len < empty.len() {
        return false;
    }
    let region = &mut bytes[start..start + len];
    region.fill(b' ');
    region[..empty.len()].copy_from_slice(empty);
    true
}

fn memfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIGHTROOM: &str = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="Adobe XMP Core 7.0">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
    xmlns:exif="http://ns.adobe.com/exif/1.0/"
    xmp:Rating="3"
    crs:Exposure2012="+0.35"
    exif:GPSLatitude="25,26.1N">
   <dc:title><rdf:Alt><rdf:li xml:lang="pt-BR">Praia</rdf:li><rdf:li xml:lang="x-default">Beach &amp; sun</rdf:li></rdf:Alt></dc:title>
   <dc:subject><rdf:Bag><rdf:li>verão</rdf:li><rdf:li>mar</rdf:li></rdf:Bag></dc:subject>
   <crs:ToneCurvePV2012><rdf:Seq><rdf:li>0, 0</rdf:li><rdf:li>255, 255</rdf:li></rdf:Seq></crs:ToneCurvePV2012>
   <exif:GPSLongitude>48,30.5W</exif:GPSLongitude>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#;

    #[test]
    fn fields_are_read_in_either_form() {
        let f = read(LIGHTROOM);
        assert_eq!(f.title.as_deref(), Some("Beach & sun"), "the default language, unescaped");
        assert_eq!(f.subject, ["verão", "mar"]);
        assert_eq!(f.rating, Some(3), "an attribute");
        assert!(f.has_gps);
        assert_eq!(read("not xml <<"), Fields::default());
    }

    #[test]
    fn a_merge_changes_only_what_it_manages() {
        let edit = Edit {
            props: vec![
                (DC, "title", Some(Value::Alt("Fim de tarde <3".into()))),
                (DC, "subject", Some(Value::Bag(vec!["praia".into()]))),
                (XMP, "Rating", Some(Value::Simple("5".into()))),
                (DC, "rights", Some(Value::Alt("© 2026 J.".into()))),
            ],
            remove_starting: vec![(EXIF, "GPS")],
        };
        let out = merge(Some(LIGHTROOM), &edit);
        let f = read(&out);
        assert_eq!(f.title.as_deref(), Some("Fim de tarde <3"));
        assert_eq!(f.subject, ["praia"]);
        assert_eq!(f.rating, Some(5));
        assert_eq!(f.rights.as_deref(), Some("© 2026 J."));
        assert!(!f.has_gps, "location gone: {out}");
        // What it does not manage is still there, as it was.
        assert!(out.contains(r#"crs:Exposure2012="+0.35""#), "{out}");
        assert!(out.contains("<rdf:li>255, 255</rdf:li>"));
        assert!(out.contains(r#"x:xmptk="Adobe XMP Core 7.0""#));
        assert!(out.starts_with("<?xpacket begin=") && out.ends_with(r#"<?xpacket end="w"?>"#));
        // Merging again changes nothing more.
        assert_eq!(read(&merge(Some(&out), &edit)), f);
    }

    #[test]
    fn a_packet_is_made_when_there_is_none_and_prefixes_never_clash() {
        let edit = Edit { props: vec![(DC, "creator", Some(Value::Seq(vec!["Ana".into(), "Bia".into()])))], remove_starting: vec![] };
        let f = read(&merge(None, &edit));
        assert_eq!(f.creator, ["Ana", "Bia"]);
        // "dc" taken by another namespace: the new one gets another prefix.
        let odd = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:dc="urn:other" dc:title="not ours"/></rdf:RDF></x:xmpmeta>"#;
        let out = merge(Some(odd), &Edit { props: vec![(DC, "title", Some(Value::Alt("Ours".into())))], remove_starting: vec![] });
        assert_eq!(read(&out).title.as_deref(), Some("Ours"));
        assert!(out.contains(r#"dc:title="not ours""#), "the other namespace's property stays: {out}");
        // Removing a value.
        let gone = merge(Some(&out), &Edit { props: vec![(DC, "title", None)], remove_starting: vec![] });
        assert_eq!(read(&gone).title, None);
    }

    #[test]
    fn a_packet_is_emptied_where_it_lies() {
        let mut bytes = b"head".to_vec();
        bytes.extend_from_slice(LIGHTROOM.as_bytes());
        bytes.extend_from_slice(b"tail");
        let len = bytes.len();
        assert!(blank_in_place(&mut bytes));
        assert_eq!(bytes.len(), len);
        assert!(bytes.starts_with(b"head") && bytes.ends_with(b"tail"));
        assert_eq!(read(&find_packet(&bytes).unwrap_or_default()), Fields::default());
        assert!(!String::from_utf8_lossy(&bytes).contains("verão"));
    }

    #[test]
    fn a_packet_is_found_inside_any_file() {
        let mut bytes = b"\x00\x01binary...".to_vec();
        bytes.extend_from_slice(LIGHTROOM.as_bytes());
        bytes.extend_from_slice(b"\xff\xd9");
        let p = find_packet(&bytes).unwrap();
        assert!(p.starts_with("<x:xmpmeta") && p.ends_with("</x:xmpmeta>"));
        assert_eq!(read(&p).rating, Some(3));
        assert!(find_packet(b"no packet here").is_none());
    }
}

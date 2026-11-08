//! A proto3 parser for what stubs need: messages, enums, services, and the mud options.
//!
//! Written after reading the proto3
//! language spec and rust-protobuf's protobuf-parse for shape; nothing copied. Errors name the line; nothing panics.
/// A field's type: the fifteen proto3 scalars by their proto names, or an enum or message by its resolved, dotted name.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(missing_docs)] // named above
pub enum Ty { Bool, Int32, Int64, Uint32, Uint64, Sint32, Sint64, Fixed32, Fixed64, Sfixed32, Sfixed64, Float, Double, String, Bytes, Enum(String), Message(String) }

/// A message field: its name, field number, type, and whether it is `repeated`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(missing_docs)] // named above
pub struct Field { pub name: String, pub number: u32, pub ty: Ty, pub repeated: bool, line: usize }

/// A message by its dotted name (`Outer.Inner` when nested), its fields in file order.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(missing_docs)] // named above
pub struct Message { pub name: String, pub fields: Vec<Field> }

/// An enum by its dotted name, its values with their numbers in file order.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(missing_docs)] // named above
pub struct Enum { pub name: String, pub values: Vec<(String, i32)> }

/// An rpc: its name, input and output message names, which side streams, and the `mud.ttl_secs` and `mud.topic` options;
/// a method with a topic is a publish, with no reply.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(missing_docs)] // named above
pub struct Method { pub name: String, pub input: String, pub output: String, pub client_stream: bool, pub server_stream: bool, pub ttl_secs: Option<u32>, pub topic: Option<String> }

/// A service, its methods in file order.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(missing_docs)] // named above
pub struct Service { pub name: String, pub methods: Vec<Method> }

/// One `.proto` file, every name resolved: what each emitter takes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[allow(missing_docs)] // named above
pub struct Schema { pub package: String, pub messages: Vec<Message>, pub enums: Vec<Enum>, pub services: Vec<Service> }

#[derive(Debug, Clone, PartialEq)]
enum Tok { Ident(String), Num(String), Str(String), Sym(char) }

type Err = (usize, String);

fn lex(text: &str) -> Result<Vec<(Tok, usize)>, Err> {
    let (mut out, mut line, mut it) = (Vec::new(), 1, text.chars().peekable());
    while let Some(&c) = it.peek() {
        match c {
            '\n' => { line += 1; it.next(); }
            c if c.is_whitespace() => { it.next(); }
            '/' => {
                it.next();
                match it.next() {
                    Some('/') => { for c in it.by_ref() { if c == '\n' { line += 1; break; } } }
                    Some('*') => { let mut last = ' '; for c in it.by_ref() { if c == '\n' { line += 1; } else if last == '*' && c == '/' { break; } last = c; } }
                    _ => return Err((line, "stray /".into())),
                }
            }
            '"' | '\'' => {
                it.next();
                let mut s = String::new();
                loop {
                    match it.next() {
                        Some(ch) if ch == c => break,
                        Some('\\') => { if let Some(e) = it.next() { s.push(match e { 'n' => '\n', 't' => '\t', other => other }); } }
                        Some('\n') | None => return Err((line, "unterminated string".into())),
                        Some(ch) => s.push(ch),
                    }
                }
                out.push((Tok::Str(s), line));
            }
            c if c.is_ascii_alphabetic() || c == '_' || c == '.' => {
                let mut s = String::new();
                while let Some(&c) = it.peek() { if c.is_ascii_alphanumeric() || c == '_' || c == '.' { s.push(c); it.next(); } else { break; } }
                out.push((Tok::Ident(s), line));
            }
            c if c.is_ascii_digit() || c == '-' => {
                let mut s = String::new();
                while let Some(&c) = it.peek() { if c.is_ascii_alphanumeric() || c == '-' || c == '.' { s.push(c); it.next(); } else { break; } }
                out.push((Tok::Num(s), line));
            }
            '{' | '}' | '(' | ')' | '[' | ']' | '<' | '>' | '=' | ';' | ',' | ':' => { out.push((Tok::Sym(c), line)); it.next(); }
            _ => return Err((line, format!("unexpected character {c:?}"))),
        }
    }
    Ok(out)
}

struct P { toks: Vec<(Tok, usize)>, i: usize, s: Schema }

impl P {
    fn line(&self) -> usize { self.toks.get(self.i).or_else(|| self.toks.last()).map_or(0, |t| t.1) }
    fn err<T>(&self, m: &str) -> Result<T, Err> { Err((self.line(), m.into())) }
    fn peek(&self) -> Option<&Tok> { self.toks.get(self.i).map(|t| &t.0) }
    fn next(&mut self) -> Option<Tok> { let t = self.toks.get(self.i).map(|t| t.0.clone()); self.i += 1; t }
    fn is(&self, t: &Tok) -> bool { self.peek() == Some(t) }
    fn is_word(&self, w: &str) -> bool { matches!(self.peek(), Some(Tok::Ident(s)) if s == w) }
    fn sym(&mut self, c: char) -> Result<(), Err> { if self.is(&Tok::Sym(c)) { self.i += 1; Ok(()) } else { self.err(&format!("expected {c:?}")) } }
    fn word(&mut self, w: &str) -> Result<(), Err> { if self.is_word(w) { self.i += 1; Ok(()) } else { self.err(&format!("expected {w}")) } }
    fn ident(&mut self) -> Result<String, Err> { if let Some(Tok::Ident(s)) = self.next() { Ok(s) } else { self.i -= 1; self.err("expected a name") } }
    fn string(&mut self) -> Result<String, Err> { if let Some(Tok::Str(s)) = self.next() { Ok(s) } else { self.i -= 1; self.err("expected a string") } }
    /// A topic travels as associated data and into three languages' literals: 1..=64 printable ASCII bytes, nothing else.
    fn topic(&mut self) -> Result<String, Err> { let s = self.string()?; if (1..=64).contains(&s.len()) && s.bytes().all(|b| (0x20..0x7f).contains(&b)) { Ok(s) } else { self.err("a topic is 1..=64 printable ASCII bytes") } }
    fn int(&mut self) -> Result<i64, Err> {
        if let Some(Tok::Num(s)) = self.next() { s.parse().or_else(|_| i64::from_str_radix(s.trim_start_matches("0x"), 16).map_err(|_| (self.line(), format!("bad number {s}")))) } else { self.i -= 1; self.err("expected a number") }
    }
    fn skip_to_semicolon(&mut self) -> Result<(), Err> { while let Some(t) = self.next() { if t == Tok::Sym(';') { return Ok(()); } } self.err("missing ;") }
    fn skip_block(&mut self) -> Result<(), Err> {
        self.sym('{')?;
        let mut depth = 1;
        while depth > 0 { match self.next() { Some(Tok::Sym('{')) => depth += 1, Some(Tok::Sym('}')) => depth -= 1, Some(_) => {}, None => return self.err("unclosed {") } }
        Ok(())
    }
    fn options(&mut self) -> Result<(), Err> { if self.is(&Tok::Sym('[')) { while self.next() != Some(Tok::Sym(']')) { if self.peek().is_none() { return self.err("unclosed ["); } } } Ok(()) }

    fn file(&mut self) -> Result<(), Err> {
        self.word("syntax")?; self.sym('=')?;
        if self.string()? != "proto3" { return self.err("only proto3"); }
        self.sym(';')?;
        while let Some(t) = self.peek().cloned() {
            match t {
                Tok::Ident(w) if w == "package" => { self.i += 1; self.s.package = self.ident()?; self.sym(';')?; }
                Tok::Ident(w) if w == "import" || w == "option" => { self.i += 1; self.skip_to_semicolon()?; }
                Tok::Ident(w) if w == "extend" => { self.i += 1; self.ident()?; self.skip_block()?; }
                Tok::Ident(w) if w == "message" => { self.i += 1; self.message("")?; }
                Tok::Ident(w) if w == "enum" => { self.i += 1; self.enumeration("")?; }
                Tok::Ident(w) if w == "service" => { self.i += 1; self.service()?; }
                Tok::Sym(';') => { self.i += 1; }
                _ => return self.err("expected message, enum, service, import, option or package"),
            }
        }
        Ok(())
    }

    fn message(&mut self, scope: &str) -> Result<(), Err> {
        if scope.matches('.').count() >= 32 { return self.err("messages nested deeper than 32"); }
        let name = format!("{scope}{}", self.ident()?);
        self.sym('{')?;
        let mut fields = Vec::new();
        loop {
            match self.peek().cloned() {
                Some(Tok::Sym('}')) => { self.i += 1; break; }
                Some(Tok::Sym(';')) => { self.i += 1; }
                Some(Tok::Ident(w)) if w == "message" => { self.i += 1; self.message(&format!("{name}."))?; }
                Some(Tok::Ident(w)) if w == "enum" => { self.i += 1; self.enumeration(&format!("{name}."))?; }
                Some(Tok::Ident(w)) if w == "option" || w == "reserved" => { self.i += 1; self.skip_to_semicolon()?; }
                Some(Tok::Ident(w)) if w == "oneof" => {
                    self.i += 1; self.ident()?; self.sym('{')?;
                    while !self.is(&Tok::Sym('}')) { if self.is_word("option") { self.i += 1; self.skip_to_semicolon()?; } else { fields.push(self.field(false)?); } }
                    self.i += 1;
                }
                Some(Tok::Ident(w)) if w == "map" => return self.err("map fields are not supported"),
                Some(Tok::Ident(w)) if w == "extensions" || w == "group" || w == "required" => return self.err(&format!("{w} is not proto3")),
                Some(Tok::Ident(_)) => { let repeated = self.is_word("repeated"); if repeated || self.is_word("optional") { self.i += 1; } fields.push(self.field(repeated)?); }
                _ => return self.err("expected a field or }"),
            }
        }
        for (i, f) in fields.iter().enumerate() {
            if fields[..i].iter().any(|g| g.number == f.number) { return Err((f.line, format!("field number {} used twice", f.number))); }
            if fields[..i].iter().any(|g| g.name == f.name) { return Err((f.line, format!("field name {} used twice", f.name))); }
        }
        self.s.messages.push(Message { name, fields });
        Ok(())
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // the number is checked against the proto range below
    fn field(&mut self, repeated: bool) -> Result<Field, Err> {
        let line = self.line();
        let ty = match self.ident()?.as_str() {
            "bool" => Ty::Bool, "int32" => Ty::Int32, "int64" => Ty::Int64, "uint32" => Ty::Uint32, "uint64" => Ty::Uint64,
            "sint32" => Ty::Sint32, "sint64" => Ty::Sint64, "fixed32" => Ty::Fixed32, "fixed64" => Ty::Fixed64,
            "sfixed32" => Ty::Sfixed32, "sfixed64" => Ty::Sfixed64, "float" => Ty::Float, "double" => Ty::Double,
            "string" => Ty::String, "bytes" => Ty::Bytes, other => Ty::Message(other.to_string()),
        };
        let name = self.ident()?;
        self.sym('=')?;
        let number = self.int()?;
        if !(1..=536_870_911).contains(&number) || (19000..=19999).contains(&number) { return Err((line, format!("field number {number} out of range"))); }
        self.options()?;
        self.sym(';')?;
        Ok(Field { name, number: number as u32, ty, repeated, line })
    }

    fn enumeration(&mut self, scope: &str) -> Result<(), Err> {
        let name = format!("{scope}{}", self.ident()?);
        self.sym('{')?;
        let mut values = Vec::new();
        while !self.is(&Tok::Sym('}')) {
            if self.is_word("option") || self.is_word("reserved") { self.i += 1; self.skip_to_semicolon()?; continue; }
            if self.is(&Tok::Sym(';')) { self.i += 1; continue; }
            let v = self.ident()?; self.sym('=')?; let n = self.int()?; self.options()?; self.sym(';')?;
            values.push((v, i32::try_from(n).map_err(|_| (self.line(), "enum value out of range".to_string()))?));
        }
        self.i += 1;
        if values.first().is_none_or(|v| v.1 != 0) { return self.err("the first enum value must be 0 in proto3"); }
        self.s.enums.push(Enum { name, values });
        Ok(())
    }

    fn service(&mut self) -> Result<(), Err> {
        let name = self.ident()?;
        self.sym('{')?;
        let mut methods = Vec::new();
        while !self.is(&Tok::Sym('}')) {
            if self.is_word("option") { self.i += 1; self.skip_to_semicolon()?; continue; }
            if self.is(&Tok::Sym(';')) { self.i += 1; continue; }
            self.word("rpc")?;
            let name = self.ident()?;
            self.sym('(')?; let client_stream = self.is_word("stream"); if client_stream { self.i += 1; } let input = self.ident()?; self.sym(')')?;
            self.word("returns")?;
            self.sym('(')?; let server_stream = self.is_word("stream"); if server_stream { self.i += 1; } let output = self.ident()?; self.sym(')')?;
            let (mut ttl_secs, mut topic) = (None, None);
            if self.is(&Tok::Sym('{')) {
                self.i += 1;
                while !self.is(&Tok::Sym('}')) {
                    if self.is(&Tok::Sym(';')) { self.i += 1; continue; }
                    self.word("option")?;
                    let key = if self.is(&Tok::Sym('(')) { self.i += 1; let k = self.ident()?; self.sym(')')?; k } else { self.ident()? };
                    self.sym('=')?;
                    match key.trim_start_matches('.') {
                        "mud.ttl_secs" => ttl_secs = Some(u32::try_from(self.int()?).map_err(|_| (self.line(), "ttl_secs out of range".to_string()))?),
                        "mud.topic" => topic = Some(self.topic()?),
                        _ => { self.next(); }
                    }
                    self.sym(';')?;
                }
                self.i += 1;
            } else { self.sym(';')?; }
            methods.push(Method { name, input, output, client_stream, server_stream, ttl_secs, topic });
        }
        self.i += 1;
        self.s.services.push(Service { name, methods });
        Ok(())
    }

    /// Type names resolve from the innermost scope outward, then as written, then without the package prefix.
    fn resolve(&self, scope: &str, name: &str) -> Option<Ty> {
        let bare = name.trim_start_matches('.');
        let bare = bare.strip_prefix(&format!("{}.", self.s.package)).unwrap_or(bare);
        let mut scopes: Vec<String> = Vec::new();
        let parts: Vec<&str> = scope.split('.').collect();
        for n in (0..parts.len()).rev() { scopes.push(parts[..=n].join(".")); }
        scopes.push(String::new());
        for s in scopes {
            let cand = if s.is_empty() { bare.to_string() } else { format!("{s}.{bare}") };
            if self.s.messages.iter().any(|m| m.name == cand) { return Some(Ty::Message(cand)); }
            if self.s.enums.iter().any(|e| e.name == cand) { return Some(Ty::Enum(cand)); }
        }
        None
    }
}

/// The schema of one `.proto` file, or the line and why it was refused.
pub fn parse(text: &str) -> Result<Schema, Err> {
    let mut p = P { toks: lex(text)?, i: 0, s: Schema::default() };
    p.file()?;
    let mut s = p.s.clone();
    for m in &mut s.messages {
        let scope = m.name.clone();
        for f in &mut m.fields {
            if let Ty::Message(n) = &f.ty { f.ty = p.resolve(&scope, n).ok_or_else(|| (f.line, format!("unknown type {n}")))?; }
        }
    }
    for svc in &s.services {
        for m in &svc.methods {
            for t in [&m.input, &m.output] {
                if p.resolve("", t).is_none_or(|t| matches!(t, Ty::Enum(_))) { return Err((0, format!("service {}: unknown message type {t}", svc.name))); }
            }
        }
    }
    Ok(s)
}

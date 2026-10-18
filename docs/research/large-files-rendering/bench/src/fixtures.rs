//! Synthetic, deterministic source files. Every unit is a few dozen lines of
//! ordinary code with varied names, strings, comments and nesting; units are
//! concatenated until the file reaches the requested size.

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const SYLLABLES: &[&str] = &[
    "ac", "bel", "cor", "dan", "el", "fen", "gor", "hal", "ix", "jun", "kel", "lor", "mar", "nov",
    "or", "pel", "quin", "ros", "sel", "tar", "ul", "ven", "wil", "xan", "yor", "zed",
];

fn word(r: &mut Rng) -> String {
    let n = 2 + r.below(2);
    let mut s = String::new();
    for _ in 0..n {
        s.push_str(SYLLABLES[r.below(SYLLABLES.len() as u64) as usize]);
    }
    s
}

fn pascal(r: &mut Rng) -> String {
    let w = word(r);
    let mut c = w.chars();
    let first = c.next().unwrap().to_ascii_uppercase();
    std::iter::once(first).chain(c).collect()
}

fn ts_unit(r: &mut Rng, out: &mut String) {
    let name = pascal(r);
    let item = pascal(r);
    let lower = word(r);
    let num = 1 + r.below(97);
    let num2 = 2 + r.below(31);
    out.push_str(&format!(
        r#"/**
 * Service for {name}: loads {item} records and keeps a small cache.
 * @see https://example.invalid/docs/{lower}
 */
export interface {name}Props {{
  id: string;
  count?: number;
  items: Array<{item}>;
  onChange: (value: {name}State) => void;
}}

export type {name}State =
  | {{ kind: "idle" }}
  | {{ kind: "loading"; progress: number }}
  | {{ kind: "done"; result: {item}[] }};

export class {name}Service extends BaseService<{name}Props> implements Disposable {{
  private readonly cache = new Map<string, {item}>();

  constructor(private readonly client: HttpClient, private options: {name}Options = {{}}) {{
    super(client);
  }}

  async load{name}(id: string, retries = 3): Promise<{item} | undefined> {{
    for (let i = 0; i < retries; i++) {{
      try {{
        const res = await this.client.get(`/api/{lower}/${{id}}?attempt=${{i}}`);
        if (res.status === 200 && res.body) {{
          const item = parse{item}(res.body as Record<string, unknown>);
          this.cache.set(id, item);
          return item;
        }}
      }} catch (err: unknown) {{
        console.warn("load{name} failed", err);
      }}
    }}
    return undefined;
  }}

  /* Clears the cache. Callers must not use the
     service after dispose(). */
  dispose(): void {{
    this.cache.clear();
  }}
}}

export function use{name}(props: {name}Props): {name}State {{
  const [state, setState] = useState<{name}State>({{ kind: "idle" }});
  const total = props.items.reduce((acc, it) => acc + (it.weight ?? 1) * {num}, 0);
  switch (state.kind) {{
    case "idle":
      setState({{ kind: "loading", progress: total / {num2} }});
      return {{ kind: "loading", progress: total / {num2} }};
    default:
      return state;
  }}
}}

"#
    ));
}

fn rust_unit(r: &mut Rng, out: &mut String) {
    let name = pascal(r);
    let field = word(r);
    let other = word(r);
    let num = 1 + r.below(97);
    out.push_str(&format!(
        r#"/// A {name} record, read from the `{field}` table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct {name} {{
    pub id: u64,
    pub {field}: String,
    pub {other}: Option<Vec<u32>>,
    flags: u8,
}}

#[derive(Debug)]
pub enum {name}Error {{
    NotFound(u64),
    Invalid {{ reason: &'static str, at: usize }},
    Io(std::io::Error),
}}

impl {name} {{
    pub fn new(id: u64, {field}: impl Into<String>) -> Self {{
        Self {{ id, {field}: {field}.into(), {other}: None, flags: 0 }}
    }}

    /// Parses one line of the form `id:{field}`.
    pub fn parse(line: &str) -> Result<Self, {name}Error> {{
        let (id, rest) = line
            .split_once(':')
            .ok_or({name}Error::Invalid {{ reason: "missing ':'", at: 0 }})?;
        let id: u64 = id.trim().parse().map_err(|_| {name}Error::Invalid {{
            reason: "bad id",
            at: line.len(),
        }})?;
        match rest {{
            "" => Err({name}Error::NotFound(id)),
            s if s.starts_with('#') => Ok(Self::new(id, &s[1..])),
            s => {{
                let mut v = Self::new(id, s);
                v.{other} = Some((0..{num}).map(|i| i * 3 + 1).collect());
                Ok(v)
            }}
        }}
    }}

    pub fn weight(&self) -> u32 {{
        self.{other}.as_ref().map_or(0, |v| v.iter().sum::<u32>() / (1 + self.flags as u32))
    }}
}}

impl std::fmt::Display for {name} {{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {{
        write!(f, "{{}}:{{}} ({{}} items)", self.id, self.{field}, self.weight())
    }}
}}

"#
    ));
}

fn json_entry(r: &mut Rng, out: &mut String, first: bool) {
    let name = word(r);
    let scope = word(r);
    let major = r.below(20);
    let minor = r.below(40);
    let patch = r.below(30);
    let mut hash = String::new();
    for _ in 0..4 {
        hash.push_str(&format!("{:016x}", r.next()));
    }
    if !first {
        out.push_str(",\n");
    }
    out.push_str(&format!(
        r#"    "node_modules/@{scope}/{name}": {{
      "version": "{major}.{minor}.{patch}",
      "resolved": "https://registry.example.invalid/@{scope}/{name}/-/{name}-{major}.{minor}.{patch}.tgz",
      "integrity": "sha512-{hash}",
      "dev": {dev},
      "license": "MIT",
      "dependencies": {{
        "{d1}": "^{major}.{minor}.0",
        "{d2}": "~{minor}.{patch}.1"
      }},
      "engines": {{
        "node": ">={node}"
      }}
    }}"#,
        dev = r.below(2) == 0,
        d1 = word(r),
        d2 = word(r),
        node = 12 + r.below(10),
    ));
}

fn minified_unit(r: &mut Rng, out: &mut String, i: usize) {
    let k = 1 + r.below(9);
    let a = word(r);
    let b = word(r);
    out.push_str(&format!(
        "function n{i}(e,t){{var r=e.length,o=[];for(var i=0;i<r;i++){{var a=e[i];if(a&&a.id!==t){{o.push({{id:a.id,v:a.v*{k},name:\"{a}-\"+i,tag:'{b}'}})}}}}return o.sort(function(x,y){{return x.v-y.v}})}}var c{i}={{key:\"{a}\",run:n{i},size:{k}e3,ok:!0,list:[1,2,3,{k}],nested:{{x:null,y:void 0}}}};"
    ));
}

pub enum Kind {
    Ts,
    Rust,
    Json,
    MinJs,
}

pub fn generate(kind: Kind, size: usize, seed: u64) -> String {
    let mut r = Rng::new(seed);
    let mut out = String::with_capacity(size + 4096);
    match kind {
        Kind::Ts => {
            out.push_str("import { BaseService, HttpClient, useState } from \"./base\";\n\n");
            while out.len() < size {
                ts_unit(&mut r, &mut out);
            }
        }
        Kind::Rust => {
            out.push_str("use std::collections::HashMap;\n\n");
            while out.len() < size {
                rust_unit(&mut r, &mut out);
            }
        }
        Kind::Json => {
            out.push_str(
                "{\n  \"name\": \"fixture\",\n  \"lockfileVersion\": 3,\n  \"packages\": {\n",
            );
            let mut first = true;
            while out.len() < size {
                json_entry(&mut r, &mut out, first);
                first = false;
            }
            out.push_str("\n  }\n}\n");
        }
        Kind::MinJs => {
            out.push_str("!function(){\"use strict\";");
            let mut i = 0;
            while out.len() < size {
                minified_unit(&mut r, &mut out, i);
                i += 1;
            }
            out.push_str("}();\n");
        }
    }
    out
}

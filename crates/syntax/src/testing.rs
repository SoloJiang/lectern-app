//! Synthetic sources for tests and benchmarks (`test-support`), in the
//! shapes of the large-file research (docs/research/large-files-rendering.md
//! 「大文件的解析与高亮」): TypeScript and Rust made of repeated units with
//! comments, template or raw strings and nested blocks; JSON shaped like a
//! `package-lock.json`; JavaScript minified into one line. Deterministic:
//! the same arguments give the same bytes.

use std::fmt::Write as _;

/// About `len` bytes of TypeScript (at least `len`, at most one unit more),
/// unit numbers starting at `seed`.
pub fn ts(len: usize, seed: usize) -> Vec<u8> {
    let mut out = String::with_capacity(len + 2048);
    out.push_str("import { Config, helper } from \"./config\";\n\n");
    let mut i = seed;
    while out.len() < len {
        let _ = write!(
            out,
            "/**\n * Service {i}: keeps the items of one queue.\n *\n * @param config the limits\n */\n\
             export class Service{i} implements Queue<Item{i}> {{\n  \
             private items: Array<{{ size: number; name: string }}> = [];\n  \
             static readonly LIMIT = {i};\n\n  \
             constructor(private readonly config: Config) {{}}\n\n  \
             /* Adds one item; the size is checked first. */\n  \
             add(item: {{ size: number; name: string }}): boolean {{\n    \
             if (item.size > this.config.limit && item.name !== \"skip\") {{\n      \
             throw new Error(`too large: ${{item.name}} (${{item.size}})`);\n    }}\n    \
             this.items.push(item);\n    return true;\n  }}\n\n  \
             describe(): string {{\n    return `\n      service {i}\n      items: ${{this.items.length}}\n    `;\n  }}\n}}\n\n\
             export interface Item{i} {{\n  readonly id: number;\n  tags?: string[];\n}}\n\n\
             export function build{i}(values: number[], scale = 1.5): Service{i} {{\n  \
             const service = new Service{i}(helper({i}));\n  \
             for (const value of values) {{\n    \
             if (value % 2 === 0) {{\n      service.add({{ size: value * scale, name: \"even\" }});\n    \
             }} else {{\n      service.add({{ size: value, name: 'odd' }}); // odd ones\n    }}\n  }}\n  \
             return service;\n}}\n\n"
        );
        i += 1;
    }
    out.into_bytes()
}

/// About `len` bytes of Rust, like [`ts`].
pub fn rust(len: usize, seed: usize) -> Vec<u8> {
    let mut out = String::with_capacity(len + 2048);
    out.push_str("use std::collections::HashMap;\n\n");
    let mut i = seed;
    while out.len() < len {
        let _ = write!(
            out,
            "/// A queue of numbered items ({i}).\n///\n/// Keeps at most `limit` of them.\n\
             #[derive(Clone, Debug, Default)]\npub struct Queue{i} {{\n    \
             items: Vec<u64>,\n    names: HashMap<String, usize>,\n    limit: usize,\n}}\n\n\
             impl Queue{i} {{\n    pub const MAX: usize = {i};\n\n    \
             /* Pushes one value; false when full. */\n    \
             pub fn push(&mut self, value: u64, name: &str) -> bool {{\n        \
             if self.items.len() >= self.limit {{\n            return false;\n        }}\n        \
             match value % 3 {{\n            0 => self.items.push(value),\n            \
             1 | 2 => {{\n                let scaled = value.saturating_mul(2);\n                \
             self.items.push(scaled);\n            }}\n            _ => unreachable!(),\n        }}\n        \
             self.names.insert(name.to_owned(), self.items.len());\n        true\n    }}\n\n    \
             pub fn report(&self) -> String {{\n        \
             format!(r#\"queue {i}: {{}} items, \"quoted\"\"#, self.items.len())\n    }}\n}}\n\n\
             pub fn drain{i}<'a>(queue: &'a mut Queue{i}, limit: u64) -> impl Iterator<Item = u64> + 'a {{\n    \
             queue.items.drain(..).filter(move |v| *v < limit) // only the small ones\n}}\n\n"
        );
        i += 1;
    }
    out.into_bytes()
}

/// About `len` bytes of JSON shaped like a `package-lock.json`: two-space
/// indentation, one object per package with nested objects and arrays.
pub fn json(len: usize, seed: usize) -> Vec<u8> {
    let mut out = String::with_capacity(len + 2048);
    out.push_str(
        "{\n  \"name\": \"fixture\",\n  \"version\": \"1.0.0\",\n  \"lockfileVersion\": 3,\n  \"requires\": true,\n  \"packages\": {\n",
    );
    let mut i = seed;
    let mut first = true;
    while out.len() < len {
        if !first {
            out.push_str(",\n");
        }
        first = false;
        let _ = write!(
            out,
            "    \"node_modules/pkg-{i}\": {{\n      \"version\": \"{}.{}.{}\",\n      \
             \"resolved\": \"https://registry.npmjs.org/pkg-{i}/-/pkg-{i}-1.0.{i}.tgz\",\n      \
             \"integrity\": \"sha512-{i:08x}abcdefABCDEF0123456789+/==\",\n      \
             \"dev\": {},\n      \"license\": \"MIT\",\n      \
             \"dependencies\": {{\n        \"pkg-{}\": \"^1.0.0\",\n        \"pkg-{}\": \"~2.1.{}\"\n      }},\n      \
             \"engines\": {{\n        \"node\": \">=18\"\n      }},\n      \
             \"bin\": [\n        \"bin/pkg-{i}.js\",\n        null,\n        {}\n      ]\n    }}",
            i % 7,
            i % 13,
            i % 101,
            i.is_multiple_of(2),
            i + 1,
            i + 2,
            i % 5,
            i * 3,
        );
        i += 1;
    }
    out.push_str("\n  }\n}\n");
    out.into_bytes()
}

/// About `len` bytes of JavaScript minified into one line (no line break,
/// no template string).
pub fn minified_js(len: usize, seed: usize) -> Vec<u8> {
    let mut out = String::with_capacity(len + 512);
    out.push_str("!function(e,t){\"use strict\";");
    let mut i = seed;
    while out.len() < len {
        let _ = write!(
            out,
            "function f{i}(a,b){{var c=a+b*{i},d=[1,2,c];if(c>10&&d.length){{return c*2}}\
             for(var k=0;k<d.length;k++){{c+=d[k]%3?d[k]:-d[k]}}\
             return\"v\"+c+'s'}}var o{i}={{key:\"k{i}\",run:function(x){{return f{i}(x,{i})}},n:{i}.5}};"
        );
        i += 1;
    }
    out.push_str("}(this,{});");
    out.into_bytes()
}

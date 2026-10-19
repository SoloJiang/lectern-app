//! Synthetic repository layouts for bench (docs/design/project.md「性能」,
//! docs/design/bench.md「夹具与 test-support」): every tracked path and the
//! contents of the manifests among them. Only the description lives here;
//! bench writes it with `lectern_git::testing::TestRepo`.

/// A repository layout: what the index lists and what the manifests say.
#[derive(Clone, Debug)]
pub struct Shape {
    /// Every tracked path, manifests included, sorted by bytes (index
    /// order), without duplicates.
    pub paths: Vec<String>,
    /// The manifests among `paths`, with their contents, sorted by path.
    pub manifests: Vec<(String, Vec<u8>)>,
    /// How many sub-projects `resolve_packages` finds (the root included).
    pub packages: usize,
}

impl Shape {
    /// The `cloudprint-miniapps` shape: 728 tracked files and 25 manifests,
    /// yarn workspaces (`packages/*`, `apps/*/web`) and a Maven
    /// multi-module build side by side; the console frontend and a code
    /// generator have a `package.json` outside the workspaces.
    pub fn cloudprint() -> Shape {
        const PACKAGES: [&str; 8] = [
            "ui", "utils", "icons", "request", "store", "hooks", "theme", "i18n",
        ];
        const WEB: [&str; 5] = ["print", "scan", "admin", "report", "device"];
        const BACKENDS: [&str; 6] = ["print", "scan", "admin", "report", "device", "console"];
        const LIBS: [&str; 2] = ["java-common", "java-sdk"];

        let mut npm: Vec<(String, String)> = Vec::new();
        let mut maven: Vec<(String, String)> = Vec::new();
        for p in PACKAGES {
            npm.push((format!("packages/{p}"), format!("@cloudprint/{p}")));
        }
        for app in WEB {
            npm.push((format!("apps/miniapp-{app}/web"), format!("{app}-web")));
        }
        npm.push((
            "apps/miniapp-console/frontend".into(),
            "console-frontend".into(),
        ));
        npm.push(("tools/codegen".into(), "codegen".into()));
        for app in BACKENDS {
            maven.push((
                format!("apps/miniapp-{app}/backend"),
                format!("{app}-backend"),
            ));
        }
        for lib in LIBS {
            maven.push((format!("libs/{lib}"), lib.into()));
        }

        let mut manifests = vec![
            (
                "package.json".to_owned(),
                package_json(
                    "cloudprint-miniapps",
                    Some(&["packages/*", "apps/*/web"]),
                    0,
                ),
            ),
            (
                "pom.xml".to_owned(),
                pom(
                    "cloudprint-parent",
                    &maven.iter().map(|(d, _)| d.as_str()).collect::<Vec<_>>(),
                ),
            ),
        ];
        for (i, (dir, name)) in npm.iter().enumerate() {
            manifests.push((format!("{dir}/package.json"), package_json(name, None, i)));
        }
        for (dir, name) in &maven {
            manifests.push((format!("{dir}/pom.xml"), pom(name, &[])));
        }

        let mut paths: Vec<String> = [
            "README.md",
            "yarn.lock",
            ".gitignore",
            ".editorconfig",
            "tsconfig.base.json",
            "docs/architecture.md",
            "docs/release.md",
            "scripts/build.sh",
        ]
        .iter()
        .map(|p| (*p).to_owned())
        .collect();
        let dirs: Vec<(&str, Eco)> = npm
            .iter()
            .map(|(d, _)| (d.as_str(), Eco::Npm))
            .chain(maven.iter().map(|(d, _)| (d.as_str(), Eco::Maven)))
            .collect();
        let others = 728 - manifests.len() - paths.len();
        for i in 0..others {
            let (dir, eco) = dirs[i % dirs.len()];
            paths.push(source_path(dir, eco, i));
        }
        // The root and one sub-project per manifest directory.
        let packages = 1 + npm.len() + maven.len();
        Shape::finish(paths, manifests, packages)
    }

    /// A large monorepo: `files` tracked files of which `manifests` (more
    /// than 4) are manifests. At the root a `package.json` with
    /// workspaces (`packages/*`, `apps/*`), a `pom.xml` with modules
    /// (`services/*`), a `go.work` and a virtual `Cargo.toml` (`crates/*`);
    /// below them one sub-project per manifest, three quarters npm, a fifth
    /// Maven, the rest Go and Cargo. The other files are spread evenly over
    /// the sub-projects. `monorepo(100_000, 2_000)` is「性能」's large
    /// repository.
    pub fn monorepo(files: usize, manifests: usize) -> Shape {
        assert!(manifests > 4 && files >= manifests, "a smaller monorepo");
        let members = manifests - 4;
        let maven = members / 5;
        let go = members / 40;
        let cargo = members / 40;
        let npm = members - maven - go - cargo;
        let apps = npm / 5;

        let mut dirs: Vec<(String, Eco)> = Vec::new();
        let mut out: Vec<(String, Vec<u8>)> = Vec::new();
        for i in 0..npm {
            let (dir, name) = if i < apps {
                (format!("apps/app-{i:03}"), format!("app-{i:03}"))
            } else {
                let n = i - apps;
                (format!("packages/pkg-{n:04}"), format!("@mono/pkg-{n:04}"))
            };
            out.push((format!("{dir}/package.json"), package_json(&name, None, i)));
            dirs.push((dir, Eco::Npm));
        }
        let mut modules = Vec::new();
        for i in 0..maven {
            let dir = format!("services/svc-{i:03}");
            out.push((format!("{dir}/pom.xml"), pom(&format!("svc-{i:03}"), &[])));
            modules.push(dir.clone());
            dirs.push((dir, Eco::Maven));
        }
        let mut uses = Vec::new();
        for i in 0..go {
            let dir = format!("tools/go-{i:02}");
            let module = format!("module example.com/mono/tools/go-{i:02}\n\ngo 1.22\n");
            out.push((format!("{dir}/go.mod"), module.into_bytes()));
            uses.push(format!("./{dir}"));
            dirs.push((dir, Eco::Go));
        }
        for i in 0..cargo {
            let dir = format!("crates/c-{i:02}");
            let toml = format!(
                "[package]\nname = \"c-{i:02}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
                 [dependencies]\nserde = \"1\"\n"
            );
            out.push((format!("{dir}/Cargo.toml"), toml.into_bytes()));
            dirs.push((dir, Eco::Cargo));
        }
        out.push((
            "package.json".into(),
            package_json("mono", Some(&["packages/*", "apps/*"]), 0),
        ));
        out.push((
            "pom.xml".into(),
            pom(
                "mono-parent",
                &modules.iter().map(String::as_str).collect::<Vec<_>>(),
            ),
        ));
        let mut work = String::from("go 1.22\n\nuse (\n");
        for u in &uses {
            work.push_str(&format!("\t{u}\n"));
        }
        work.push_str(")\n");
        out.push(("go.work".into(), work.into_bytes()));
        out.push((
            "Cargo.toml".into(),
            b"[workspace]\nmembers = [\"crates/*\"]\nresolver = \"2\"\n".to_vec(),
        ));

        let mut paths: Vec<String> = vec!["README.md".into(), ".gitignore".into()];
        let others = files - manifests - paths.len().min(files - manifests);
        for i in 0..others {
            let (dir, eco) = &dirs[i % dirs.len()];
            paths.push(source_path(dir, *eco, i));
        }
        Shape::finish(paths, out, 1 + members)
    }

    fn finish(
        mut paths: Vec<String>,
        mut manifests: Vec<(String, Vec<u8>)>,
        packages: usize,
    ) -> Shape {
        manifests.sort_by(|a, b| a.0.cmp(&b.0));
        paths.extend(manifests.iter().map(|(p, _)| p.clone()));
        paths.sort();
        paths.dedup();
        Shape {
            paths,
            manifests,
            packages,
        }
    }
}

#[derive(Clone, Copy)]
enum Eco {
    Npm,
    Maven,
    Go,
    Cargo,
}

/// The `i`-th source file of a sub-project, two directories deep.
fn source_path(dir: &str, eco: Eco, i: usize) -> String {
    let d = i % 7;
    match eco {
        Eco::Npm if i.is_multiple_of(2) => format!("{dir}/src/components/d{d}/Item{i:05}.tsx"),
        Eco::Npm => format!("{dir}/src/lib/d{d}/item{i:05}.ts"),
        Eco::Maven => format!("{dir}/src/main/java/com/example/d{d}/Item{i:05}.java"),
        Eco::Go => format!("{dir}/internal/d{d}/item{i:05}.go"),
        Eco::Cargo => format!("{dir}/src/d{d}/item{i:05}.rs"),
    }
}

/// About 600 bytes, like a small real `package.json`: scripts and a few
/// dependencies.
fn package_json(name: &str, workspaces: Option<&[&str]>, i: usize) -> Vec<u8> {
    let workspaces = workspaces.map_or_else(String::new, |w| {
        let list: Vec<String> = w.iter().map(|p| format!("\"{p}\"")).collect();
        format!("  \"workspaces\": [{}],\n", list.join(", "))
    });
    format!(
        r#"{{
  "name": "{name}",
  "version": "1.{i}.0",
  "private": true,
{workspaces}  "main": "dist/index.js",
  "types": "dist/index.d.ts",
  "scripts": {{
    "build": "tsc -p tsconfig.json",
    "dev": "vite --port {port}",
    "lint": "eslint src --ext .ts,.tsx",
    "test": "vitest run"
  }},
  "dependencies": {{
    "react": "^18.3.1",
    "react-dom": "^18.3.1",
    "dayjs": "^1.11.10",
    "zustand": "^4.5.2"
  }},
  "devDependencies": {{
    "typescript": "^5.4.5",
    "vite": "^5.2.11",
    "vitest": "^1.6.0",
    "eslint": "^8.57.0"
  }}
}}
"#,
        port = 3000 + i % 1000,
    )
    .into_bytes()
}

/// About 700 bytes, like a small real `pom.xml`.
fn pom(artifact: &str, modules: &[&str]) -> Vec<u8> {
    let mut modules_xml = String::new();
    if !modules.is_empty() {
        modules_xml.push_str("  <modules>\n");
        for m in modules {
            modules_xml.push_str(&format!("    <module>{m}</module>\n"));
        }
        modules_xml.push_str("  </modules>\n");
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <parent>
    <groupId>com.example</groupId>
    <artifactId>parent</artifactId>
    <version>1.0.0</version>
  </parent>
  <artifactId>{artifact}</artifactId>
  <packaging>jar</packaging>
{modules_xml}  <properties>
    <java.version>17</java.version>
  </properties>
  <dependencies>
    <dependency>
      <groupId>org.springframework.boot</groupId>
      <artifactId>spring-boot-starter-web</artifactId>
    </dependency>
    <dependency>
      <groupId>org.junit.jupiter</groupId>
      <artifactId>junit-jupiter</artifactId>
      <scope>test</scope>
    </dependency>
  </dependencies>
</project>
"#
    )
    .into_bytes()
}

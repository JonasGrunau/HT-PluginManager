//! `hurentohn.json`, the file each plug-in repo keeps at its root so the
//! manager can show and build it. Repos without one fall back to their README.

use serde::Deserialize;

pub const FILE: &str = "hurentohn.json";

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Manifest {
    /// Format version of this file. 1 today.
    pub schema: u32,
    /// Product name as hosts show it, e.g. `PhaseFucker`.
    pub name: String,
    /// What the plug-in does, in a few words: the editor's sub-line category.
    pub category: String,
    /// One or two sentences for the plug-in list.
    pub description: String,
    /// Product picture: a path in the repo (`docs/screenshot.png`) or an https URL.
    pub image: String,
    /// The bundles a release installs, by file name.
    pub bundles: Vec<String>,
    pub build: Build,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Build {
    /// Extra `-D` flags for the CMake configure step.
    pub cmake_args: Vec<String>,
    /// CMake targets that produce the bundles.
    pub targets: Vec<String>,
    /// Where the targets put their bundles, relative to the build dir.
    /// VST3s land in `<artefacts>/VST3`, Audio Units in `<artefacts>/AU`.
    pub artefacts: String,
}

impl Manifest {
    pub fn parse(bytes: &[u8], repo_name: &str) -> Result<Self, String> {
        let mut m: Manifest = serde_json::from_slice(bytes).map_err(|e| format!("{FILE} is not valid: {e}"))?;
        m.fill_defaults(repo_name);
        m.check()?;
        Ok(m)
    }

    /// Names that end up in install commands must be plain: every bundle a
    /// bare `.vst3`/`.component` file name, the artefacts folder inside the
    /// build dir.
    pub fn check(&self) -> Result<(), String> {
        if let Some(bad) = self.bundles.iter().find(|b| !crate::installed::valid_bundle_name(b)) {
            return Err(format!("{FILE}: bundle {bad:?} is not a plain .vst3 or .component file name."));
        }
        if self.bundles.is_empty() {
            return Err(format!("{FILE} names no bundles."));
        }
        let inside = std::path::Path::new(&self.build.artefacts).components().all(|c| matches!(c, std::path::Component::Normal(_)));
        if !inside {
            return Err(format!("{FILE}: artefacts {:?} must be a folder inside the build dir.", self.build.artefacts));
        }
        Ok(())
    }

    /// No manifest: take the README's title, first paragraph and first image.
    pub fn from_readme(readme: &str, repo_name: &str) -> Self {
        let mut m = Manifest::default();
        let mut paragraph: Vec<&str> = Vec::new();
        let mut in_code = false;
        for line in readme.lines() {
            let t = line.trim();
            if t.starts_with("```") {
                in_code = !in_code;
                continue;
            }
            if in_code {
                continue;
            }
            if m.name.is_empty()
                && let Some(title) = t.strip_prefix("# ")
            {
                m.name = title.trim().to_string();
                continue;
            }
            if m.image.is_empty()
                && let Some(path) = markdown_image(t)
            {
                m.image = path;
            }
            if m.description.is_empty() {
                let prose = !t.is_empty() && !t.starts_with('#') && !t.starts_with('!') && !t.starts_with('<') && !t.starts_with('|');
                if prose {
                    paragraph.push(t);
                } else if !paragraph.is_empty() {
                    m.description = strip_markdown(&paragraph.join(" "));
                }
            }
        }
        if m.description.is_empty() && !paragraph.is_empty() {
            m.description = strip_markdown(&paragraph.join(" "));
        }
        m.fill_defaults(repo_name);
        // A README title like "A/B Tester" makes no usable bundle name; the
        // repo name always does.
        if m.check().is_err() {
            let name = std::mem::take(&mut m.name);
            m.bundles.clear();
            m.build = Build::default();
            m.fill_defaults(repo_name);
            m.name = name;
        }
        m
    }

    fn fill_defaults(&mut self, repo_name: &str) {
        if self.name.is_empty() {
            self.name = repo_name.to_string();
        }
        let n = self.name.clone();
        if self.bundles.is_empty() {
            self.bundles = vec![format!("{n}.vst3"), format!("{n}.component")];
        }
        if self.build.targets.is_empty() {
            self.build.targets = vec![format!("{n}_VST3"), format!("{n}_AU")];
        }
        if self.build.artefacts.is_empty() {
            self.build.artefacts = format!("{n}_artefacts/Release");
        }
    }

    /// The name split for the `LEFT/RIGHT` wordmark: `PhaseFucker` → `PHASE`, `FUCKER`.
    pub fn wordmark(&self) -> (String, String) {
        let split = self.name.char_indices().skip(1).find(|(_, c)| c.is_uppercase()).map(|(i, _)| i);
        match split {
            Some(i) => (self.name[..i].to_uppercase(), self.name[i..].to_uppercase()),
            None => (self.name.to_uppercase(), String::new()),
        }
    }
}

fn markdown_image(line: &str) -> Option<String> {
    let start = line.find("![")?;
    let rest = &line[start..];
    let open = rest.find("](")? + 2;
    let close = rest[open..].find(')')? + open;
    let target = rest[open..close].split_whitespace().next()?;
    Some(target.to_string())
}

/// Drops `**`, `*`, `` ` `` and turns `[text](url)` into `text`.
fn strip_markdown(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('[') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match (after.find("]("), after.find(')')) {
            (Some(close), Some(end)) if close < end => {
                out.push_str(&after[..close]);
                rest = &after[end + 1..];
            }
            _ => {
                out.push('[');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.replace("**", "").replace(['`', '*'], "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readme_fallback_takes_title_paragraph_and_picture() {
        let readme = "# PhaseFucker\n\nAutomatic phase alignment for two\ntracks, see [docs](x.md). Press **ALIGN**.\n\n![PhaseFucker](docs/screenshot.png)\n";
        let m = Manifest::from_readme(readme, "PhaseFucker");
        assert_eq!(m.name, "PhaseFucker");
        assert_eq!(m.description, "Automatic phase alignment for two tracks, see docs. Press ALIGN.");
        assert_eq!(m.image, "docs/screenshot.png");
        assert_eq!(m.bundles, ["PhaseFucker.vst3", "PhaseFucker.component"]);
        assert_eq!(m.build.targets, ["PhaseFucker_VST3", "PhaseFucker_AU"]);
    }

    #[test]
    fn wordmark_splits_at_the_second_capital() {
        let m = |n: &str| Manifest { name: n.into(), ..Default::default() }.wordmark();
        assert_eq!(m("BuildApp"), ("BUILD".into(), "APP".into()));
        assert_eq!(m("DisTitten"), ("DIS".into(), "TITTEN".into()));
        assert_eq!(m("plain"), ("PLAIN".into(), String::new()));
    }

    #[test]
    fn manifest_rejects_paths_in_names() {
        assert!(Manifest::parse(br#"{"bundles":[".."]}"#, "X").is_err());
        assert!(Manifest::parse(br#"{"bundles":["../../evil.vst3"]}"#, "X").is_err());
        assert!(Manifest::parse(br#"{"build":{"artefacts":"../../etc"}}"#, "X").is_err());
        assert!(Manifest::parse(br#"{"build":{"artefacts":"/etc"}}"#, "X").is_err());
        assert!(Manifest::parse(br#"{"bundles":["X.vst3"]}"#, "X").is_ok());
    }

    #[test]
    fn readme_titles_that_are_no_file_names_fall_back_to_the_repo() {
        let m = Manifest::from_readme("# A/B Tester\n\nCompares two things.\n", "ABTester");
        assert_eq!(m.name, "A/B Tester");
        assert_eq!(m.bundles, ["ABTester.vst3", "ABTester.component"]);
        assert_eq!(m.build.artefacts, "ABTester_artefacts/Release");
    }

    #[test]
    fn manifest_fills_build_defaults() {
        let m = Manifest::parse(br#"{"schema":1,"name":"VocAnal","category":"Vocal chain"}"#, "VocAnal").unwrap();
        assert_eq!(m.build.artefacts, "VocAnal_artefacts/Release");
        assert_eq!(m.category, "Vocal chain");
    }
}

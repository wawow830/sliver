use super::*;
use std::fs;

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("sliver-icons-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn file(&self, rel: &str) {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, "x").unwrap();
    }

    fn index(&self, theme: &str, body: &str) {
        let p = self.root.join("icons").join(theme).join("index.theme");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    fn loader(&self, theme: &str) -> IconLoader {
        IconLoader::new(
            vec![self.root.join("icons")],
            vec![self.root.join("pixmaps")],
            theme,
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

const HICOLOR: &str = "[Icon Theme]\nName=Hicolor\nDirectories=16x16/apps,48x48/apps,scalable/apps\n\n\
[16x16/apps]\nSize=16\nType=Fixed\n\n[48x48/apps]\nSize=48\nType=Fixed\n\n\
[scalable/apps]\nSize=64\nMinSize=8\nMaxSize=512\nType=Scalable\n";

fn rel(i: &Icon, root: &Path) -> String {
    i.path
        .strip_prefix(root.join("icons"))
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

#[test]
fn exact_then_closest_size() {
    let f = Fixture::new("size");
    f.index("hicolor", HICOLOR);
    f.file("icons/hicolor/16x16/apps/a.png");
    f.file("icons/hicolor/48x48/apps/a.png");
    let l = f.loader("hicolor");
    assert_eq!(
        rel(&l.find("a", 16).unwrap(), &f.root),
        "hicolor/16x16/apps/a.png"
    );
    assert_eq!(
        rel(&l.find("a", 48).unwrap(), &f.root),
        "hicolor/48x48/apps/a.png"
    );
    // 40 is closer to 48 than to 16; 20 closer to 16
    assert_eq!(
        rel(&l.find("a", 40).unwrap(), &f.root),
        "hicolor/48x48/apps/a.png"
    );
    assert_eq!(
        rel(&l.find("a", 20).unwrap(), &f.root),
        "hicolor/16x16/apps/a.png"
    );
    assert_eq!(l.find("a", 40).unwrap().size, 48);
}

#[test]
fn svg_in_scalable_matches_any_size_and_beats_png_in_same_dir() {
    let f = Fixture::new("svg");
    f.index("hicolor", HICOLOR);
    f.file("icons/hicolor/scalable/apps/b.svg");
    f.file("icons/hicolor/scalable/apps/b.png");
    f.file("icons/hicolor/16x16/apps/b.png");
    let l = f.loader("hicolor");
    let i = l.find("b", 100).unwrap();
    assert_eq!((i.kind, i.scalable), (IconKind::Svg, true));
    // an exact fixed match wins over a scalable one only in dir order; here 16x16 is listed first
    let i = l.find("b", 16).unwrap();
    assert_eq!(rel(&i, &f.root), "hicolor/16x16/apps/b.png");
}

#[test]
fn inherits_in_order_then_hicolor() {
    let f = Fixture::new("inherit");
    f.index("hicolor", HICOLOR);
    f.index(
        "child",
        "[Icon Theme]\nName=C\nInherits=parent1,parent2\nDirectories=16x16/apps\n[16x16/apps]\nSize=16\n",
    );
    f.index(
        "parent1",
        "[Icon Theme]\nName=P1\nInherits=grand\nDirectories=16x16/apps\n[16x16/apps]\nSize=16\n",
    );
    f.index(
        "parent2",
        "[Icon Theme]\nName=P2\nDirectories=16x16/apps\n[16x16/apps]\nSize=16\n",
    );
    f.index(
        "grand",
        "[Icon Theme]\nName=G\nDirectories=16x16/apps\n[16x16/apps]\nSize=16\n",
    );
    f.file("icons/child/16x16/apps/only-child.png");
    f.file("icons/parent2/16x16/apps/both.png");
    f.file("icons/grand/16x16/apps/both.png");
    f.file("icons/grand/16x16/apps/deep.png");
    f.file("icons/hicolor/16x16/apps/fallback.png");
    let l = f.loader("child");
    assert!(rel(&l.find("only-child", 16).unwrap(), &f.root).starts_with("child/"));
    // parent1's parent (grand) is searched before parent2: depth-first
    assert!(rel(&l.find("both", 16).unwrap(), &f.root).starts_with("grand/"));
    assert!(rel(&l.find("deep", 16).unwrap(), &f.root).starts_with("grand/"));
    assert!(rel(&l.find("fallback", 16).unwrap(), &f.root).starts_with("hicolor/"));
    assert_eq!(l.find("missing", 16), None);
}

#[test]
fn inherit_cycles_terminate() {
    let f = Fixture::new("cycle");
    f.index(
        "a",
        "[Icon Theme]\nName=A\nInherits=b\nDirectories=d\n[d]\nSize=16\n",
    );
    f.index(
        "b",
        "[Icon Theme]\nName=B\nInherits=a\nDirectories=d\n[d]\nSize=16\n",
    );
    assert_eq!(f.loader("a").find("nothing", 16), None);
}

#[test]
fn threshold_directories() {
    let f = Fixture::new("thr");
    f.index(
        "t",
        "[Icon Theme]\nName=T\nDirectories=a,b\n[a]\nSize=32\nThreshold=8\n[b]\nSize=64\nType=Fixed\n",
    );
    f.file("icons/t/a/x.png");
    f.file("icons/t/b/x.png");
    let l = f.loader("t");
    // 38 is within 32±8 -> exact match in `a`
    assert_eq!(rel(&l.find("x", 38).unwrap(), &f.root), "t/a/x.png");
    // 60: not in a's range, b is Fixed 64 (distance 4) vs a (distance 20)
    assert_eq!(rel(&l.find("x", 60).unwrap(), &f.root), "t/b/x.png");
}

#[test]
fn scale_matching() {
    let f = Fixture::new("scale");
    f.index(
        "s",
        "[Icon Theme]\nName=S\nDirectories=16\nScaledDirectories=16@2\n[16]\nSize=16\nType=Fixed\n[16@2]\nSize=16\nScale=2\nType=Fixed\n",
    );
    f.file("icons/s/16/x.png");
    f.file("icons/s/16@2/x.png");
    let l = f.loader("s");
    assert_eq!(
        rel(&l.lookup(&Query::new("x", 16)).unwrap(), &f.root),
        "s/16/x.png"
    );
    let two = l.lookup(&Query::new("x", 16).scale(2)).unwrap();
    assert_eq!(
        (rel(&two, &f.root).as_str(), two.scale),
        ("s/16@2/x.png", 2)
    );
}

#[test]
fn symbolic_preference_and_dash_fallback() {
    let f = Fixture::new("sym");
    f.index("hicolor", HICOLOR);
    f.file("icons/hicolor/16x16/apps/audio-volume-high.png");
    f.file("icons/hicolor/16x16/apps/audio-volume-high-symbolic.png");
    f.file("icons/hicolor/16x16/apps/network.png");
    let l = f.loader("hicolor");
    assert!(rel(&l.find("audio-volume-high", 16).unwrap(), &f.root).ends_with("high.png"));
    let s = l
        .lookup(&Query::new("audio-volume-high", 16).symbolic())
        .unwrap();
    assert!(rel(&s, &f.root).ends_with("high-symbolic.png"));
    // no symbolic variant -> plain one
    let n = l.lookup(&Query::new("network", 16).symbolic()).unwrap();
    assert!(rel(&n, &f.root).ends_with("network.png"));
    // generic fallback: network-wired-disconnected -> network
    assert!(
        rel(&l.find("network-wired-disconnected", 16).unwrap(), &f.root).ends_with("network.png")
    );
}

#[test]
fn unthemed_pixmaps_and_absolute_paths() {
    let f = Fixture::new("pix");
    f.index("hicolor", HICOLOR);
    f.file("pixmaps/legacy.xpm");
    f.file("pixmaps/both.png");
    f.file("pixmaps/both.svg");
    f.file("loose.png");
    let l = f.loader("hicolor");
    let i = l.find("legacy", 32).unwrap();
    assert_eq!((i.kind, i.size), (IconKind::Xpm, 0));
    assert_eq!(l.find("both", 32).unwrap().kind, IconKind::Svg);
    let abs = f.root.join("loose.png");
    let i = l.find(abs.to_str().unwrap(), 32).unwrap();
    assert_eq!(i.path, abs);
    assert_eq!(l.find("/no/such/file.png", 32), None);
    assert_eq!(l.find("/etc/passwd", 32), None, "unknown extension");
}

#[test]
fn per_query_theme_cache_and_clear() {
    let f = Fixture::new("cache");
    f.index("hicolor", HICOLOR);
    f.index(
        "other",
        "[Icon Theme]\nName=O\nDirectories=16x16/apps\n[16x16/apps]\nSize=16\n",
    );
    f.file("icons/hicolor/16x16/apps/z.png");
    let l = f.loader("hicolor");
    assert!(
        rel(
            &l.lookup(&Query::new("z", 16).theme("other")).unwrap(),
            &f.root
        )
        .starts_with("hicolor/"),
        "falls through to hicolor"
    );
    assert_eq!(l.find("late", 16), None);
    f.file("icons/hicolor/16x16/apps/late.png");
    assert_eq!(l.find("late", 16), None, "negative result is cached");
    l.clear_cache();
    assert!(l.find("late", 16).is_some());
    assert_eq!(l.installed_themes(), ["hicolor", "other"]);
}

#[test]
fn configured_theme_sources() {
    let f = Fixture::new("cfg");
    let p = SearchPaths {
        config_home: Some(f.root.join("cfg")),
        ..Default::default()
    };
    assert_eq!(configured_theme(&p), None);
    fs::create_dir_all(f.root.join("cfg")).unwrap();
    fs::write(
        f.root.join("cfg/kdeglobals"),
        "[Icons]\nTheme=breeze-dark\n",
    )
    .unwrap();
    assert_eq!(configured_theme(&p).as_deref(), Some("breeze-dark"));
    fs::create_dir_all(f.root.join("cfg/gtk-3.0")).unwrap();
    fs::write(
        f.root.join("cfg/gtk-3.0/settings.ini"),
        "[Settings]\ngtk-icon-theme-name=Adwaita\n",
    )
    .unwrap();
    assert_eq!(
        configured_theme(&p).as_deref(),
        Some("Adwaita"),
        "GTK wins over KDE"
    );
    fs::create_dir_all(f.root.join("cfg/gtk-4.0")).unwrap();
    fs::write(
        f.root.join("cfg/gtk-4.0/settings.ini"),
        "[Settings]\ngtk-icon-theme-name=\"Papirus\"\n",
    )
    .unwrap();
    assert_eq!(configured_theme(&p).as_deref(), Some("Papirus"));
}

#[test]
fn names_cover_the_theme_chain_hicolor_and_pixmaps() {
    let f = Fixture::new("names");
    f.index("hicolor", HICOLOR);
    f.index(
        "child",
        "[Icon Theme]\nName=C\nInherits=parent\nDirectories=scalable/apps\n\n[scalable/apps]\nSize=64\nType=Scalable\n",
    );
    f.index(
        "parent",
        "[Icon Theme]\nName=P\nDirectories=scalable/apps\n\n[scalable/apps]\nSize=64\nType=Scalable\n",
    );
    f.index("other", HICOLOR);
    f.file("icons/child/scalable/apps/mine.svg");
    f.file("icons/parent/scalable/apps/inherited.png");
    f.file("icons/parent/scalable/apps/notes.txt");
    f.file("icons/hicolor/48x48/apps/fallback.png");
    f.file("icons/hicolor/16x16/apps/fallback.png");
    f.file("icons/other/48x48/apps/not-in-chain.png");
    f.file("pixmaps/loose.xpm");
    let l = f.loader("child");
    assert_eq!(
        l.names(None),
        ["fallback", "inherited", "loose", "mine"]
            .map(String::from)
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
    );
    assert!(l.names(Some("other")).contains(&"not-in-chain".to_string()));
    assert!(!l.names(Some("other")).contains(&"mine".to_string()));
}

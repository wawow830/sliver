//! The emoji picker strip and its search row.

use super::*;

/// The emoji picker as a row of cells: group tabs, one page of emoji, a close button.
/// Tap an emoji to type it, tap a tab to switch group, swipe to page, ✕ to close the layer.
/// Hold an emoji that has skin tones to choose one: the page becomes ‹, the plain emoji and its
/// five tones. Group pages show each emoji in the tone last picked for it (from recents).
pub(super) fn emoji_strip(
    m: &ModuleInstance,
    key: &str,
    world: &World,
    ui: &EmojiUi,
    out: &mut Vec<Cell>,
) {
    use sliver_emoji::Group;
    let cols = m
        .config
        .get("columns")
        .and_then(|v| v.as_integer())
        .map_or(14, |n| n.clamp(4, 40) as usize);
    let insert = m
        .config
        .get("insert")
        .and_then(|v| v.as_str())
        .unwrap_or("auto");
    let recents = world.emoji.items();
    // An empty recents tab shows the first group rather than a blank bar.
    let tab = if ui.tab == 0 && recents.is_empty() {
        1
    } else {
        ui.tab.min(Group::ALL.len())
    };
    // (text to show and type, the plain emoji when it has tones)
    let items: Vec<(String, Option<&'static str>)> = if tab == 0 {
        recents
            .iter()
            .map(|t| {
                let fam = sliver_emoji::family(t).filter(|e| e.tones.is_some());
                (t.clone(), fam.map(|e| e.text))
            })
            .collect()
    } else {
        sliver_emoji::in_group(Group::ALL[tab - 1])
            .map(|e| {
                let has = e.tones.is_some();
                (world.emoji.preferred(e), has.then_some(e.text))
            })
            .collect()
    };
    let offset = ui.offset.min(items.len().saturating_sub(cols));
    let bare = |k: String, text: &str, w: f32, classes: &[&str], on: Vec<(Gesture, Action)>| Cell {
        key: k,
        id: m.id.to_string(),
        kind: "emoji".into(),
        mic: false,
        slider: false,
        css_id: m.css_id.clone(),
        classes: classes
            .iter()
            .map(|c| c.to_string())
            .chain(m.classes.iter().cloned())
            .collect(),
        width: Width::Fixed(w),
        content: Content::markup(sliver_render::markup::escape(text)),
        touch: Touch::Agent,
        on,
        next_change: None,
        pressed: false,
        checked: false,
        drawer: None,
        disabled: false,
        foreground: None,
        background: None,
        published: None,
        pass_through: false,
        target: None,
        timer_total: None,
        menu: Vec::new(),
        slot: None,
        spectrum: Vec::new(),
    };
    let act = |name: String| Action::ModuleAction(name);
    if let Some(query) = ui.search.as_ref().filter(|_| ui.tones.is_none()) {
        search_strip(m, key, world, ui, query, insert, &bare, out);
        return;
    }
    for t in 0..=Group::ALL.len() {
        let icon = if t == 0 {
            "🕘"
        } else {
            Group::ALL[t - 1].icon()
        };
        let mut c = bare(
            format!("{key}/tab{t}"),
            icon,
            52.0,
            &["emoji-tab"],
            vec![(Gesture::Tap, act(format!("tab:{t}")))],
        );
        c.checked = t == tab;
        out.push(c);
    }
    out.push(bare(
        format!("{key}/search"),
        "🔍",
        52.0,
        &["emoji-tab", "emoji-search"],
        vec![(Gesture::Tap, act("search".into()))],
    ));
    let tones = ui
        .tones
        .as_deref()
        .and_then(sliver_emoji::family)
        .and_then(|e| Some((e.text, e.tones?)));
    if let Some((plain, variants)) = tones {
        out.push(bare(
            format!("{key}/back"),
            "‹",
            52.0,
            &["emoji-back"],
            vec![(Gesture::Tap, act("tones-close".into()))],
        ));
        for (i, text) in std::iter::once(plain).chain(variants).enumerate() {
            out.push(bare(
                format!("{key}/t{i}"),
                text,
                76.0,
                &["emoji", "emoji-tone"],
                vec![(Gesture::Tap, act(format!("pick:{insert}:{text}")))],
            ));
        }
    } else {
        for (i, (text, plain)) in items.iter().enumerate().skip(offset).take(cols) {
            let mut on = vec![
                (Gesture::Tap, act(format!("pick:{insert}:{text}"))),
                (Gesture::SwipeLeft, act(format!("page:{}", cols))),
                (Gesture::SwipeRight, act(format!("page:-{}", cols))),
            ];
            if let Some(p) = plain {
                on.push((Gesture::Hold, act(format!("tones:{p}"))));
            }
            out.push(bare(format!("{key}/e{i}"), text, 76.0, &["emoji"], on));
        }
    }
    out.push(bare(
        format!("{key}/close"),
        "✕",
        52.0,
        &["emoji-close"],
        vec![(Gesture::Tap, act("close".into()))],
    ));
}

/// How many results fit next to the keys.
pub(super) const SEARCH_RESULTS: usize = 6;

pub(super) type Bare<'a> = dyn Fn(String, &str, f32, &[&str], Vec<(Gesture, Action)>) -> Cell + 'a;

/// Emoji search: no keyboard is grabbed. A row of letters (and a space) builds the query on the
/// bar itself; the matches (names, word prefixes) sit at the right, swipe to page. ⌫ erases,
/// tapping the query clears it, ✕ goes back to the groups.
#[allow(clippy::too_many_arguments)]
pub(super) fn search_strip(
    m: &ModuleInstance,
    key: &str,
    world: &World,
    ui: &EmojiUi,
    query: &str,
    insert: &str,
    bare: &Bare<'_>,
    out: &mut Vec<Cell>,
) {
    let act = |name: String| Action::ModuleAction(name);
    let shown = if query.is_empty() {
        "🔍".to_string()
    } else {
        format!("🔍 {query}")
    };
    out.push(bare(
        format!("{key}/query"),
        &shown,
        150.0,
        &["emoji-query"],
        vec![(Gesture::Tap, act("s-clear".into()))],
    ));
    for c in "abcdefghijklmnopqrstuvwxyz ".chars() {
        let label = if c == ' ' {
            "␣".to_string()
        } else {
            c.to_string()
        };
        out.push(bare(
            format!("{key}/k{}", if c == ' ' { '_' } else { c }),
            &label,
            36.0,
            &["emoji-key"],
            vec![(Gesture::Tap, act(format!("s:{c}")))],
        ));
    }
    out.push(bare(
        format!("{key}/back"),
        "⌫",
        52.0,
        &["emoji-key", "emoji-back"],
        vec![(Gesture::Tap, act("s-back".into()))],
    ));
    let hits = sliver_emoji::search(query, 120);
    let offset = ui.offset.min(hits.len().saturating_sub(SEARCH_RESULTS));
    for (i, e) in hits.iter().enumerate().skip(offset).take(SEARCH_RESULTS) {
        let text = world.emoji.preferred(e);
        let mut on = vec![
            (Gesture::Tap, act(format!("pick:{insert}:{text}"))),
            (Gesture::SwipeLeft, act(format!("page:{SEARCH_RESULTS}"))),
            (Gesture::SwipeRight, act(format!("page:-{SEARCH_RESULTS}"))),
        ];
        if e.tones.is_some() {
            on.push((Gesture::Hold, act(format!("tones:{}", e.text))));
        }
        out.push(bare(format!("{key}/e{i}"), &text, 64.0, &["emoji"], on));
    }
    if !query.is_empty() && hits.is_empty() {
        out.push(bare(
            format!("{key}/none"),
            "no match",
            140.0,
            &["emoji-none"],
            Vec::new(),
        ));
    }
    let _ = m;
    out.push(bare(
        format!("{key}/close"),
        "✕",
        52.0,
        &["emoji-close"],
        vec![(Gesture::Tap, act("s-close".into()))],
    ));
}

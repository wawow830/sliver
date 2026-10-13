//! The icon picker (ux.md §4 Look): a dialog with three tabs: the icon theme (searchable, with
//! the icons built into the bar), an emoji list, and a file chooser. It only reports a [`Choice`];
//! the inspector decides which keys to write.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

/// What was picked.
pub enum Choice {
    /// A theme icon name, built-in name, or file path: the module's `icon`.
    Icon(String),
    /// An emoji: drawn as text, so it becomes the module's `format` (and `icon` is cleared).
    Emoji(String),
}

/// How many results a grid shows at once. A full theme has thousands of names; searching narrows.
const LIMIT: usize = 120;

/// Icon names to offer: the bar's built-ins first (they always exist), then the theme's.
pub fn icon_names(theme: Option<&str>) -> Vec<String> {
    let mut names: Vec<String> = sliver_render::BUILTIN_ICONS
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let seen: std::collections::HashSet<String> = names.iter().cloned().collect();
    names.extend(
        sliver_icons::IconLoader::from_env()
            .names(theme)
            .into_iter()
            .filter(|n| !seen.contains(n)),
    );
    names
}

/// Names containing every word of `query` (case-insensitive), prefix matches first.
pub fn filter(names: &[String], query: &str, limit: usize) -> Vec<String> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut hits: Vec<(bool, &String)> = names
        .iter()
        .filter_map(|n| {
            let l = n.to_lowercase();
            words
                .iter()
                .all(|w| l.contains(w.as_str()))
                .then(|| (words.first().is_none_or(|w| l.starts_with(w.as_str())), n))
        })
        .collect();
    // Stable: prefix matches first, the original order otherwise.
    hits.sort_by_key(|(prefix, _)| !prefix);
    hits.into_iter()
        .take(limit)
        .map(|(_, n)| n.clone())
        .collect()
}

/// `#rrggbb` of the widget's text colour.
fn hex(c: gtk::gdk::RGBA) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(c.red()), b(c.green()), b(c.blue()))
}

fn picture(name: &str, theme: Option<&str>, fg: &str) -> gtk::Widget {
    let loader = sliver_icons::IconLoader::from_env();
    let image = if let Some(svg) = sliver_render::Icon::builtin_source(name) {
        // The built-ins are white; draw them in the dialog's text colour.
        let svg = String::from_utf8_lossy(svg)
            .replace("#FFFFFF", fg)
            .replace("fill=\"white\"", &format!("fill=\"{fg}\""));
        let icon = gio::BytesIcon::new(&glib::Bytes::from_owned(svg.into_bytes()));
        gtk::Image::from_gicon(&icon)
    } else {
        let mut q = sliver_icons::Query::new(name, 32);
        if let Some(t) = theme {
            q = q.theme(t);
        }
        match loader.lookup(&q) {
            Some(i) => gtk::Image::from_file(&i.path),
            None => gtk::Image::from_icon_name("image-missing"),
        }
    };
    image.set_pixel_size(32);
    image.upcast()
}

fn tile(content: &gtk::Widget, label: &str, tooltip: &str) -> gtk::Button {
    let b = gtk::Button::builder().tooltip_text(tooltip).build();
    b.add_css_class("flat");
    let v = gtk::Box::new(gtk::Orientation::Vertical, 2);
    v.append(content);
    let l = gtk::Label::builder()
        .label(label)
        .max_width_chars(12)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .build();
    l.add_css_class("caption");
    v.append(&l);
    b.set_child(Some(&v));
    b
}

fn grid() -> (gtk::FlowBox, gtk::ScrolledWindow) {
    let flow = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .homogeneous(true)
        .min_children_per_line(3)
        .max_children_per_line(8)
        .valign(gtk::Align::Start)
        .build();
    let scroll = gtk::ScrolledWindow::builder()
        .child(&flow)
        .vexpand(true)
        .min_content_height(320)
        .build();
    (flow, scroll)
}

fn clear(flow: &gtk::FlowBox) {
    while let Some(c) = flow.first_child() {
        flow.remove(&c);
    }
}

/// Opens the picker over `parent`. `theme` is the module's `icon-theme`, if any.
pub fn open(parent: &impl IsA<gtk::Widget>, theme: Option<String>, pick: Rc<dyn Fn(Choice)>) {
    let dialog = adw::Dialog::builder()
        .title("Choose an icon")
        .content_width(520)
        .content_height(520)
        .build();
    let stack = adw::ViewStack::new();
    let fg = hex(parent.color());

    // Icons.
    let names = Rc::new(icon_names(theme.as_deref()));
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search icons")
        .build();
    let (flow, scroll) = grid();
    let more = gtk::Label::new(None);
    more.add_css_class("dim-label");
    let show: Rc<dyn Fn(&str)> = {
        let (flow, names, theme, more, dialog, pick, fg) = (
            flow.clone(),
            names.clone(),
            theme.clone(),
            more.clone(),
            dialog.clone(),
            pick.clone(),
            fg.clone(),
        );
        Rc::new(move |q: &str| {
            clear(&flow);
            let hits = filter(&names, q, LIMIT);
            for n in &hits {
                let b = tile(&picture(n, theme.as_deref(), &fg), n, n);
                let (n, pick, dialog) = (n.clone(), pick.clone(), dialog.clone());
                b.connect_clicked(move |_| {
                    pick(Choice::Icon(n.clone()));
                    dialog.close();
                });
                flow.append(&b);
            }
            let total = names.len();
            more.set_text(&if hits.len() == LIMIT {
                format!("First {LIMIT} of {total}. Type to narrow.")
            } else if hits.is_empty() {
                "Nothing matches.".to_owned()
            } else {
                String::new()
            });
        })
    };
    show("");
    {
        let show = show.clone();
        search.connect_search_changed(move |e| show(e.text().as_str()));
    }
    let icons = gtk::Box::new(gtk::Orientation::Vertical, 6);
    icons.set_margin_top(6);
    icons.set_margin_bottom(6);
    icons.set_margin_start(12);
    icons.set_margin_end(12);
    icons.append(&search);
    icons.append(&scroll);
    icons.append(&more);
    stack
        .add_titled(&icons, Some("icons"), "Icons")
        .set_icon_name(Some("applications-graphics-symbolic"));

    // Emoji.
    let esearch = gtk::SearchEntry::builder()
        .placeholder_text("Search emoji by name")
        .build();
    let (eflow, escroll) = grid();
    let group = Rc::new(RefCell::new(sliver_emoji::Group::ALL[0]));
    let groups = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    groups.set_halign(gtk::Align::Center);
    let eshow: Rc<dyn Fn()> = {
        let (eflow, esearch, group, dialog, pick) = (
            eflow.clone(),
            esearch.clone(),
            group.clone(),
            dialog.clone(),
            pick.clone(),
        );
        Rc::new(move || {
            clear(&eflow);
            let q = esearch.text();
            let list: Vec<&sliver_emoji::Emoji> = if q.trim().is_empty() {
                sliver_emoji::in_group(*group.borrow()).collect()
            } else {
                sliver_emoji::search(q.as_str(), 200)
            };
            for e in list {
                let l = gtk::Label::new(Some(e.text));
                l.add_css_class("title-2");
                let b = gtk::Button::builder()
                    .child(&l)
                    .tooltip_text(e.name)
                    .build();
                b.add_css_class("flat");
                let (text, pick, dialog) = (e.text.to_owned(), pick.clone(), dialog.clone());
                b.connect_clicked(move |_| {
                    pick(Choice::Emoji(text.clone()));
                    dialog.close();
                });
                eflow.append(&b);
            }
        })
    };
    for g in sliver_emoji::Group::ALL {
        let b = gtk::Button::builder()
            .label(g.icon())
            .tooltip_text(g.name())
            .build();
        b.add_css_class("flat");
        let (group, eshow, esearch) = (group.clone(), eshow.clone(), esearch.clone());
        b.connect_clicked(move |_| {
            *group.borrow_mut() = g;
            esearch.set_text("");
            eshow();
        });
        groups.append(&b);
    }
    eshow();
    {
        let eshow = eshow.clone();
        esearch.connect_search_changed(move |_| eshow());
    }
    let note = gtk::Label::new(Some(
        "An emoji is drawn as the module's label (it sets Format).",
    ));
    note.add_css_class("dim-label");
    let emoji = gtk::Box::new(gtk::Orientation::Vertical, 6);
    emoji.set_margin_top(6);
    emoji.set_margin_bottom(6);
    emoji.set_margin_start(12);
    emoji.set_margin_end(12);
    emoji.append(&esearch);
    emoji.append(&groups);
    emoji.append(&escroll);
    emoji.append(&note);
    stack
        .add_titled(&emoji, Some("emoji"), "Emoji")
        .set_icon_name(Some("face-smile-symbolic"));

    // File.
    let file_page = adw::StatusPage::builder()
        .icon_name("document-open-symbolic")
        .title("An image file")
        .description("PNG or SVG. The path is stored as it is, so keep the file where it is.")
        .build();
    let choose = gtk::Button::builder()
        .label("Choose a File…")
        .halign(gtk::Align::Center)
        .build();
    choose.add_css_class("suggested-action");
    choose.add_css_class("pill");
    {
        let (dialog, pick) = (dialog.clone(), pick.clone());
        choose.connect_clicked(move |b| {
            let filter = gtk::FileFilter::new();
            filter.set_name(Some("Images"));
            for m in ["image/png", "image/svg+xml"] {
                filter.add_mime_type(m);
            }
            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            let d = gtk::FileDialog::builder()
                .title("Icon file")
                .filters(&filters)
                .build();
            let window = b.root().and_downcast::<gtk::Window>();
            let (dialog, pick) = (dialog.clone(), pick.clone());
            d.open(window.as_ref(), gio::Cancellable::NONE, move |r| {
                if let Ok(f) = r
                    && let Some(p) = f.path()
                {
                    pick(Choice::Icon(p.display().to_string()));
                    dialog.close();
                }
            });
        });
    }
    file_page.set_child(Some(&choose));
    stack
        .add_titled(&file_page, Some("file"), "File")
        .set_icon_name(Some("folder-pictures-symbolic"));

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(
        &adw::ViewSwitcher::builder()
            .stack(&stack)
            .policy(adw::ViewSwitcherPolicy::Wide)
            .build(),
    ));
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&stack));
    dialog.set_child(Some(&view));
    dialog.present(Some(parent));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Vec<String> {
        [
            "audio-volume-high",
            "volume_up",
            "battery-full",
            "video-display",
            "Volume-Extra",
        ]
        .map(String::from)
        .to_vec()
    }

    #[test]
    fn filter_matches_words_anywhere_and_prefixes_first() {
        assert_eq!(
            filter(&names(), "vol", 10),
            ["volume_up", "Volume-Extra", "audio-volume-high"]
        );
        assert_eq!(filter(&names(), "audio high", 10), ["audio-volume-high"]);
        assert!(filter(&names(), "zzz", 10).is_empty());
    }

    #[test]
    fn filter_keeps_order_with_no_query_and_limits() {
        assert_eq!(filter(&names(), "", 2), ["audio-volume-high", "volume_up"]);
    }
}

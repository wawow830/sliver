//! The brightness-curve editor (T75, GOAL Appearance): how bright the bar is for each brightness
//! of the main display, in adaptive mode. It edits `[display] curve = [[screen, bar], …]`
//! (fractions 0–1; bar is a fraction of max-brightness; T74). The editor works in percent and
//! writes hundredths. Without a curve, sliverd uses tiny-dfr's square root, drawn dashed.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use sliver_config::Display;
use toml::Value;

use crate::{Ui, apply};

const W: f64 = 520.0;
const H: f64 = 220.0;
/// Room for the axis labels.
const LEFT: f64 = 40.0;
const BOTTOM: f64 = 26.0;
const TOP: f64 = 10.0;
const RIGHT: f64 = 20.0;
/// How close (px) a press must be to grab a point.
const GRAB: f64 = 12.0;
/// sliver-config accepts 2 to 16 points.
const MAX_POINTS: usize = 16;

type Points = Vec<(f64, f64)>;

/// tiny-dfr's adaptive brightness, as percent of max: `sqrt(screen)`.
fn default_at(screen: f64) -> f64 {
    (screen / 100.0).clamp(0.0, 1.0).sqrt() * 100.0
}

/// Piecewise-linear through `pts` (sorted by screen), flat outside the ends.
fn curve_at(pts: &[(f64, f64)], screen: f64) -> f64 {
    let Some(first) = pts.first() else {
        return default_at(screen);
    };
    if screen <= first.0 {
        return first.1;
    }
    for w in pts.windows(2) {
        let ((x0, y0), (x1, y1)) = (w[0], w[1]);
        if screen <= x1 {
            return if x1 > x0 {
                y0 + (y1 - y0) * (screen - x0) / (x1 - x0)
            } else {
                y1
            };
        }
    }
    pts.last().map_or(0.0, |p| p.1)
}

/// The config's curve in percent, if one is set (an invalid one is reported by the validator and
/// means the default).
pub fn read(d: &Display) -> Option<Points> {
    (d.curve.len() >= 2).then(|| {
        d.curve
            .iter()
            .map(|&(s, b)| ((s as f64 * 100.0).round(), (b as f64 * 100.0).round()))
            .collect()
    })
}

/// Where to start editing when there's no curve: the default, sampled.
fn starting_points() -> Points {
    [0.0, 25.0, 50.0, 75.0, 100.0]
        .iter()
        .map(|&x| (x, default_at(x).round()))
        .collect()
}

/// Sorted by screen, whole percents, 0–100, and no two at the same screen value.
fn normalized(mut pts: Points) -> Points {
    for p in &mut pts {
        *p = (p.0.round().clamp(0.0, 100.0), p.1.round().clamp(0.0, 100.0));
    }
    pts.sort_by(|a, b| a.0.total_cmp(&b.0));
    pts.dedup_by(|b, a| a.0 == b.0);
    pts
}

fn to_value(pts: &[(f64, f64)]) -> Value {
    Value::Array(
        pts.iter()
            .map(|&(x, y)| Value::Array(vec![Value::Float(x / 100.0), Value::Float(y / 100.0)]))
            .collect(),
    )
}

fn px(x: f64, y: f64) -> (f64, f64) {
    (
        LEFT + x / 100.0 * (W - LEFT - RIGHT),
        TOP + (1.0 - y / 100.0) * (H - TOP - BOTTOM),
    )
}

fn unpx(px: f64, py: f64) -> (f64, f64) {
    (
        ((px - LEFT) / (W - LEFT - RIGHT) * 100.0).clamp(0.0, 100.0),
        ((1.0 - (py - TOP) / (H - TOP - BOTTOM)) * 100.0).clamp(0.0, 100.0),
    )
}

fn nearest(pts: &[(f64, f64)], x: f64, y: f64) -> Option<usize> {
    pts.iter()
        .enumerate()
        .map(|(i, &(a, b))| {
            let (pa, pb) = px(a, b);
            (i, (pa - x).hypot(pb - y))
        })
        .filter(|(_, d)| *d <= GRAB)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

fn save(ui: &Rc<Ui>, pts: Option<Points>) {
    apply(ui, move |ed, _| match pts {
        Some(p) => {
            ed.set("display.curve", &to_value(&normalized(p)))?;
            Ok("Brightness curve changed".into())
        }
        None => {
            ed.remove("display.curve")?;
            Ok("Brightness curve: back to the default".into())
        }
    });
}

fn draw(cr: &gtk::cairo::Context, pts: &[(f64, f64)], custom: bool, fg: &gtk::gdk::RGBA) {
    let (r, g, b) = (fg.red() as f64, fg.green() as f64, fg.blue() as f64);
    cr.set_line_width(1.0);
    // Grid and labels every 25%.
    cr.set_font_size(11.0);
    for i in 0..=4 {
        let v = i as f64 * 25.0;
        cr.set_source_rgba(r, g, b, 0.12);
        let (x0, y) = px(0.0, v);
        let (x1, _) = px(100.0, v);
        cr.move_to(x0, y);
        cr.line_to(x1, y);
        let (x, y0) = px(v, 0.0);
        let (_, y1) = px(v, 100.0);
        cr.move_to(x, y0);
        cr.line_to(x, y1);
        let _ = cr.stroke();
        cr.set_source_rgba(r, g, b, 0.55);
        let label = format!("{v:.0}%");
        cr.move_to(4.0, px(0.0, v).1 + 4.0);
        let _ = cr.show_text(&label);
        cr.move_to(px(v, 0.0).0 - 12.0, H - 8.0);
        let _ = cr.show_text(&label);
    }
    // The default (√), dashed.
    cr.set_source_rgba(r, g, b, if custom { 0.35 } else { 0.0 });
    cr.set_dash(&[4.0, 4.0], 0.0);
    for s in 0..=100 {
        let (x, y) = px(s as f64, default_at(s as f64));
        if s == 0 {
            cr.move_to(x, y);
        } else {
            cr.line_to(x, y);
        }
    }
    let _ = cr.stroke();
    cr.set_dash(&[], 0.0);
    // The curve in effect.
    let accent = (0.21, 0.52, 0.89);
    cr.set_source_rgb(accent.0, accent.1, accent.2);
    cr.set_line_width(2.5);
    // Until a point moves, what's in effect is the smooth default, not the handles' polyline.
    let in_effect = |s: f64| {
        if custom {
            curve_at(pts, s)
        } else {
            default_at(s)
        }
    };
    for s in 0..=100 {
        let (x, y) = px(s as f64, in_effect(s as f64));
        if s == 0 {
            cr.move_to(x, y);
        } else {
            cr.line_to(x, y);
        }
    }
    let _ = cr.stroke();
    for &(a, b) in pts {
        let (x, y) = px(a, b);
        cr.arc(x, y, 5.5, 0.0, std::f64::consts::TAU);
        cr.set_source_rgb(1.0, 1.0, 1.0);
        let _ = cr.fill_preserve();
        cr.set_source_rgb(accent.0, accent.1, accent.2);
        cr.set_line_width(2.0);
        let _ = cr.stroke();
    }
}

/// The editor row: the graph, plus a Reset button when a curve is set.
pub fn editor(ui: &Rc<Ui>, display: &Display) -> gtk::Widget {
    let adaptive = display.adaptive;
    let set = read(display);
    let custom = set.is_some();
    let pts: Rc<RefCell<Points>> = Rc::new(RefCell::new(set.unwrap_or_else(starting_points)));
    let dragging: Rc<Cell<Option<usize>>> = Rc::default();

    let area = gtk::DrawingArea::builder()
        .content_width(W as i32)
        .content_height(H as i32)
        .halign(gtk::Align::Center)
        .sensitive(adaptive)
        .build();
    {
        let pts = pts.clone();
        area.set_draw_func(move |a, cr, _, _| draw(cr, &pts.borrow(), custom, &a.color()));
    }

    // Drag a point; it follows the pointer (kept between its neighbours) and is saved on release.
    let drag = gtk::GestureDrag::new();
    {
        let (pts, dragging) = (pts.clone(), dragging.clone());
        drag.connect_drag_begin(move |_, x, y| dragging.set(nearest(&pts.borrow(), x, y)));
    }
    {
        let (pts, dragging, area2) = (pts.clone(), dragging.clone(), area.clone());
        drag.connect_drag_update(move |g, dx, dy| {
            let (Some(i), Some((x0, y0))) = (dragging.get(), g.start_point()) else {
                return;
            };
            let (mut x, y) = unpx(x0 + dx, y0 + dy);
            let mut p = pts.borrow_mut();
            let lo = if i == 0 { 0.0 } else { p[i - 1].0 + 1.0 };
            let hi = if i + 1 == p.len() {
                100.0
            } else {
                p[i + 1].0 - 1.0
            };
            x = x.clamp(lo, hi.max(lo));
            p[i] = (x, y);
            drop(p);
            area2.queue_draw();
        });
    }
    {
        let (ui, pts, dragging) = (ui.clone(), pts.clone(), dragging.clone());
        drag.connect_drag_end(move |_, dx, dy| {
            if dragging.take().is_some() && (dx.abs() > 1.0 || dy.abs() > 1.0) {
                save(&ui, Some(pts.borrow().clone()));
            }
        });
    }
    area.add_controller(drag);

    // Double-click adds a point on the curve; right-click removes one (two always stay).
    let click = gtk::GestureClick::builder().button(0).build();
    {
        let (ui, pts) = (ui.clone(), pts.clone());
        click.connect_pressed(move |g, n, x, y| {
            let mut p = pts.borrow().clone();
            if g.current_button() == 3 {
                if let Some(i) = nearest(&p, x, y)
                    && p.len() > 2
                {
                    p.remove(i);
                    save(&ui, Some(p));
                }
            } else if n == 2 && nearest(&p, x, y).is_none() {
                if p.len() >= MAX_POINTS {
                    return ui.toast(&format!("A curve has at most {MAX_POINTS} points"));
                }
                let (sx, _) = unpx(x, y);
                p.push((sx, curve_at(&p, sx)));
                save(&ui, Some(p));
            }
        });
    }
    area.add_controller(click);

    let col = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_hexpand(true);
    text.append(
        &gtk::Label::builder()
            .label("Brightness curve")
            .xalign(0.0)
            .build(),
    );
    let sub = gtk::Label::builder()
        .label(if adaptive {
            "Bar brightness (% of the maximum) for each main-display brightness. Drag points; double-click adds one, right-click removes one."
        } else {
            "Used with adaptive brightness."
        })
        .xalign(0.0)
        .wrap(true)
        .build();
    sub.add_css_class("dim-label");
    sub.add_css_class("caption");
    text.append(&sub);
    head.append(&text);
    if custom {
        let reset = gtk::Button::builder()
            .label("Reset")
            .tooltip_text("Back to the default curve (square root, like tiny-dfr)")
            .valign(gtk::Align::Center)
            .build();
        let ui = ui.clone();
        reset.connect_clicked(move |_| save(&ui, None));
        head.append(&reset);
    }
    col.append(&head);
    col.append(&area);
    adw::PreferencesRow::builder()
        .title("Brightness curve")
        .activatable(false)
        .child(&col)
        .build()
        .upcast()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolation_and_normalizing() {
        let p = vec![(0.0, 10.0), (50.0, 30.0), (100.0, 100.0)];
        assert_eq!(curve_at(&p, 25.0), 20.0);
        assert_eq!(curve_at(&p, 75.0), 65.0);
        assert_eq!(
            curve_at(&[(20.0, 5.0), (80.0, 50.0)], 0.0),
            5.0,
            "flat below"
        );
        assert_eq!(curve_at(&[], 25.0), 50.0, "no curve: sqrt");
        let n = normalized(vec![(60.4, 101.0), (10.0, -3.0), (60.0, 7.0)]);
        assert_eq!(n, [(10.0, 0.0), (60.0, 100.0)]);
        let c = sliver_config::load_str(
            "[display]\ncurve = [[0, 0.05], [1, 0.9]]\n",
            std::path::Path::new("."),
        );
        assert!(c.diagnostics.ok(), "{:?}", c.diagnostics);
        assert_eq!(
            read(&c.config.display),
            Some(vec![(0.0, 5.0), (100.0, 90.0)])
        );
        // What the editor writes loads (fractions, strictly increasing).
        let text = format!(
            "[display]\ncurve = {}\n",
            to_value(&normalized(vec![(51.0, 26.0), (0.0, 0.0), (100.0, 100.0)]))
        );
        let c = sliver_config::load_str(&text, std::path::Path::new("."));
        assert!(c.diagnostics.ok(), "{text}: {:?}", c.diagnostics);
        assert_eq!(
            read(&c.config.display),
            Some(vec![(0.0, 0.0), (51.0, 26.0), (100.0, 100.0)])
        );
        assert_eq!(read(&Display::default()), None);
    }
}

//! Styles: what the stylesheet says for each item of a scene, cached per scene.

use super::*;

impl Renderer {
    pub(super) fn compute_styles(&mut self, scene: &Scene, sheet: &Stylesheet) -> Styles {
        if self.styles_generation != sheet.generation() || self.styles.len() > 4096 {
            self.styles.clear();
            self.styles_generation = sheet.generation();
        }
        let bar_el = Element {
            tag: "bar",
            id: None,
            classes: &scene.classes,
            active: false,
            disabled: false,
            checked: false,
            first: true,
            last: true,
        };
        let mut key = format!("bar.{}", scene.classes.join("."));
        let bar = self.cached(&key, sheet, &[bar_el], None);
        let mut zones = Vec::new();
        let mut items = Vec::new();
        let mut parts = Vec::new();
        let mut spectra = Vec::new();
        let nz = scene.zones.len();
        for (zi, zone) in scene.zones.iter().enumerate() {
            // A zone's name is a class (`.zone-main`, like waybar's `.modules-left`), so `#id`
            // always means a module.
            let mut zclasses = Vec::with_capacity(zone.classes.len() + 1);
            zclasses.extend(zone.name.iter().map(|n| format!("zone-{n}")));
            zclasses.extend(zone.classes.iter().cloned());
            let zone_el = Element {
                tag: "zone",
                id: None,
                classes: &zclasses,
                active: false,
                disabled: false,
                checked: false,
                first: zi == 0,
                last: zi + 1 == nz,
            };
            let zone_key = format!(
                "{key}>zone.{}:{}{}",
                zclasses.join("."),
                zone_el.first as u8,
                zone_el.last as u8
            );
            let zs = self.cached(&zone_key, sheet, &[bar_el, zone_el], Some(&bar));
            let ni = zone.items.len();
            for (ii, item) in zone.items.iter().enumerate() {
                // Waybar themes select every module with `.module`.
                let mut classes = Vec::with_capacity(item.classes.len() + 1);
                classes.push("module".to_string());
                classes.extend(item.classes.iter().cloned());
                let el = Element {
                    tag: "module",
                    id: item.id.as_deref(),
                    classes: &classes,
                    active: item.pressed,
                    disabled: item.disabled,
                    checked: item.checked,
                    first: ii == 0,
                    last: ii + 1 == ni,
                };
                key.clear();
                key.push_str(&zone_key);
                key.push_str(&format!(
                    ">module#{}.{}:{}{}{}{}{}",
                    item.id.as_deref().unwrap_or(""),
                    classes.join("."),
                    el.active as u8,
                    el.disabled as u8,
                    el.checked as u8,
                    el.first as u8,
                    el.last as u8
                ));
                let ms = self.cached(&key, sheet, &[bar_el, zone_el, el], Some(&zs));
                parts.push(if matches!(item.content, Content::Slider { .. }) {
                    let child = |tag| Element {
                        tag,
                        id: None,
                        classes: &[],
                        active: item.pressed,
                        disabled: item.disabled,
                        checked: item.checked,
                        first: true,
                        last: tag != "highlight",
                    };
                    let (trough, hl, knob) = (child("trough"), child("highlight"), child("slider"));
                    let tk = format!("{key}>trough");
                    let ts = self.cached(&tk, sheet, &[bar_el, zone_el, el, trough], Some(&ms));
                    let hs = self.cached(
                        &format!("{tk}>highlight"),
                        sheet,
                        &[bar_el, zone_el, el, trough, hl],
                        Some(&ts),
                    );
                    let ks = self.cached(
                        &format!("{tk}>slider"),
                        sheet,
                        &[bar_el, zone_el, el, trough, knob],
                        Some(&ts),
                    );
                    Some([ts, hs, ks])
                } else {
                    None
                });
                spectra.push(if item.spectrum.is_empty() {
                    None
                } else {
                    let child = Element {
                        tag: "spectrum",
                        id: None,
                        classes: &[],
                        active: item.pressed,
                        disabled: item.disabled,
                        checked: item.checked,
                        first: true,
                        last: true,
                    };
                    Some(self.cached(
                        &format!("{key}>spectrum"),
                        sheet,
                        &[bar_el, zone_el, el, child],
                        Some(&ms),
                    ))
                });
                items.push(ms);
            }
            zones.push(zs);
            key = format!("bar.{}", scene.classes.join("."));
        }
        Styles {
            bar,
            zones,
            items,
            parts,
            spectra,
        }
    }

    pub(super) fn cached(
        &mut self,
        key: &str,
        sheet: &Stylesheet,
        path: &[Element<'_>],
        parent: Option<&Style>,
    ) -> Arc<Style> {
        if let Some(s) = self.styles.get(key) {
            return s.clone();
        }
        let s = Arc::new(sheet.compute(path, parent));
        self.styles.insert(key.to_string(), s.clone());
        s
    }
}

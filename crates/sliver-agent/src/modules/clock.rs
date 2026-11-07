//! The `clock` module's placeholders and when it next needs a redraw.

use super::*;

pub(super) const CLOCK_DEFAULT: &str = "{:%H:%M}";

/// Clock placeholders: `{}`/`{time}` with a strftime spec, like waybar (`{:%a %d %b}`), and
/// `{timezone}`. waybar's `timezone`/`timezones` (`""` is local time; swipe to cycle) and `locale`
/// (`de_DE`; names of days and months) are read from the module block.
pub(super) struct Clock {
    now: DateTime<Local>,
    /// `None`: local time. `Err`: a name that isn't in the time-zone database.
    zone: Option<Result<chrono_tz::Tz, String>>,
    locale: Option<Result<chrono::Locale, String>>,
}

impl Clock {
    /// The clock for this module, and how many time zones it cycles through.
    pub(super) fn new(m: &ModuleInstance, now: DateTime<Local>, tz: i32) -> (Clock, usize) {
        let zones: Vec<String> = match (m.texts("timezones"), m.text("timezone")) {
            (z, _) if !z.is_empty() => z.to_vec(),
            (_, Some(z)) => vec![z.to_owned()],
            _ => Vec::new(),
        };
        let zone = (!zones.is_empty())
            .then(|| &zones[tz.rem_euclid(zones.len() as i32) as usize])
            .filter(|z| !z.is_empty())
            .map(|z| z.parse::<chrono_tz::Tz>().map_err(|_| z.clone()));
        let locale = m.text("locale").map(|l| {
            // `de_DE.UTF-8@euro` → `de_DE`, as found in $LANG.
            let base = l.split(['.', '@']).next().unwrap_or(l);
            chrono::Locale::try_from(base).map_err(|_| l.to_owned())
        });
        (Clock { now, zone, locale }, zones.len())
    }
}

impl Values for Clock {
    fn format(&self, name: &str, spec: Option<&str>) -> Option<String> {
        match name {
            "" | "time" => {
                let spec = spec.unwrap_or("%H:%M");
                let items: Vec<FmtItem> = StrftimeItems::new(spec).collect();
                if items.iter().any(|i| matches!(i, FmtItem::Error)) {
                    return Some(format!("⚠ bad time format {spec:?}"));
                }
                let locale = match &self.locale {
                    None => None,
                    Some(Ok(l)) => Some(*l),
                    Some(Err(l)) => return Some(format!("⚠ unknown locale {l:?}")),
                };
                Some(match &self.zone {
                    Some(Err(z)) => format!("⚠ unknown time zone {z:?}"),
                    Some(Ok(tz)) => strftime(&self.now.with_timezone(tz), &items, locale),
                    None => strftime(&self.now, &items, locale),
                })
            }
            "timezone" => Some(match &self.zone {
                None => "local".to_owned(),
                Some(Ok(tz)) => tz.name().to_owned(),
                Some(Err(z)) => z.clone(),
            }),
            _ => None,
        }
    }
}

pub(super) fn strftime<Tz: chrono::TimeZone>(
    t: &DateTime<Tz>,
    items: &[FmtItem],
    locale: Option<chrono::Locale>,
) -> String
where
    Tz::Offset: std::fmt::Display,
{
    match locale {
        Some(l) => t.format_localized_with_items(items.iter(), l).to_string(),
        None => t.format_with_items(items.iter()).to_string(),
    }
}

/// The next second if the format shows seconds, else the next minute.
pub(super) fn next_clock_tick(now: DateTime<Local>, fmt: &str) -> DateTime<Local> {
    let seconds = ["%S", "%T", "%s", "%X", "%r", "%c", "%+"]
        .iter()
        .any(|s| fmt.contains(s));
    let base = now.with_nanosecond(0).unwrap_or(now);
    if seconds {
        base + chrono::Duration::seconds(1)
    } else {
        base.with_second(0).unwrap_or(base) + chrono::Duration::minutes(1)
    }
}

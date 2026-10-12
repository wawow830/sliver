use std::path::{Path, PathBuf};

/// One temperature sensor.
#[derive(Debug, Clone, PartialEq)]
pub struct Sensor {
    /// `<chip>/<label>` (hwmon) or `<zone type>` (thermal zone).
    pub name: String,
    pub celsius: f64,
    /// The sensor's own critical threshold, if it reports one.
    pub critical: Option<f64>,
    pub path: PathBuf,
}

fn read(p: &Path) -> Option<String> {
    std::fs::read_to_string(p)
        .ok()
        .map(|s| s.trim().to_string())
}

fn millidegrees(p: &Path) -> Option<f64> {
    read(p)?.parse::<f64>().ok().map(|m| m / 1000.0)
}

/// hwmon `temp*_input` files, then thermal zones, under `sys` (`/sys`), sorted by name.
pub fn sensors(sys: &Path) -> Vec<Sensor> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(sys.join("class/hwmon")) {
        for chip in rd.filter_map(Result::ok) {
            let dir = chip.path();
            let chip_name = read(&dir.join("name"))
                .unwrap_or_else(|| chip.file_name().to_string_lossy().into_owned());
            let Ok(files) = std::fs::read_dir(&dir) else {
                continue;
            };
            for f in files.filter_map(Result::ok) {
                let fname = f.file_name().to_string_lossy().into_owned();
                let Some(stem) = fname
                    .strip_prefix("temp")
                    .and_then(|r| r.strip_suffix("_input"))
                else {
                    continue;
                };
                let Some(celsius) = millidegrees(&f.path()) else {
                    continue;
                };
                let label = read(&dir.join(format!("temp{stem}_label")))
                    .unwrap_or_else(|| format!("temp{stem}"));
                out.push(Sensor {
                    name: format!("{chip_name}/{label}"),
                    celsius,
                    critical: millidegrees(&dir.join(format!("temp{stem}_crit"))),
                    path: f.path(),
                });
            }
        }
    }
    if let Ok(rd) = std::fs::read_dir(sys.join("class/thermal")) {
        for z in rd.filter_map(Result::ok) {
            let dir = z.path();
            if !z.file_name().to_string_lossy().starts_with("thermal_zone") {
                continue;
            }
            let Some(celsius) = millidegrees(&dir.join("temp")) else {
                continue;
            };
            out.push(Sensor {
                name: read(&dir.join("type"))
                    .unwrap_or_else(|| z.file_name().to_string_lossy().into_owned()),
                celsius,
                critical: None,
                path: dir.join("temp"),
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

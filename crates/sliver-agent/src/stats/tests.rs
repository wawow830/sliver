use super::*;
use std::fs;

const STAT_A: &str = "cpu  100 0 100 800 0 0 0 0 0 0\ncpu0 50 0 50 400 0 0 0 0 0 0\nintr 1\n";
const STAT_B: &str = "cpu  200 0 200 900 0 0 0 0 0 0\ncpu0 100 0 100 450 0 0 0 0 0 0\n";

#[test]
fn cpu_line_and_usage() {
    let a = parse_cpu_line(STAT_A).unwrap();
    assert_eq!((a.busy, a.idle), (200, 800));
    let b = parse_cpu_line(STAT_B).unwrap();
    assert_eq!(
        b.usage_since(a),
        200.0 * 100.0 / 300.0,
        "200 busy of 300 new jiffies"
    );
    assert_eq!(a.usage_since(a), 0.0, "no time passed");
    assert!(parse_cpu_line("garbage").is_none());
    // iowait counts as idle, steal as busy
    let c = parse_cpu_line("cpu 10 0 10 10 70 0 0 0\n").unwrap();
    assert_eq!((c.busy, c.idle), (20, 80));
}

#[test]
fn meminfo() {
    let m = parse_meminfo("MemTotal: 16000 kB\nMemFree: 100 kB\nMemAvailable: 4000 kB\nSwapTotal: 2000 kB\nSwapFree: 500 kB\n").unwrap();
    assert_eq!((m.total, m.available), (16000 * 1024, 4000 * 1024));
    assert_eq!(m.used(), 12000 * 1024);
    assert_eq!(m.percent_used(), 75.0);
    assert_eq!(m.swap_percent(), 75.0);
    let old = parse_meminfo("MemTotal: 100 kB\nMemFree: 40 kB\n").unwrap();
    assert_eq!(old.available, 40 * 1024, "falls back to MemFree");
    assert_eq!(old.swap_percent(), 0.0);
    assert!(parse_meminfo("nothing").is_none());
}

#[test]
fn loadavg() {
    let l = parse_loadavg("0.52 1.25 2.00 1/300 12345\n").unwrap();
    assert_eq!((l.one, l.five, l.fifteen), (0.52, 1.25, 2.0));
    assert!(parse_loadavg("x").is_none());
}

struct Fixture(std::path::PathBuf);

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let d = std::env::temp_dir().join(format!("sliver-stats-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("proc")).unwrap();
        fs::create_dir_all(d.join("sys")).unwrap();
        Fixture(d)
    }
    fn write(&self, rel: &str, text: &str) {
        let p = self.0.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn sampler_cpu_window_and_frequency() {
    let f = Fixture::new("cpu");
    f.write("proc/stat", STAT_A);
    f.write(
        "sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq",
        "2000000\n",
    );
    f.write(
        "sys/devices/system/cpu/cpu1/cpufreq/scaling_cur_freq",
        "3000000\n",
    );
    f.write("sys/devices/system/cpu/cpufreq/policy0", "x"); // not a cpuN dir
    let mut s = Sampler::with_roots(f.0.join("proc"), f.0.join("sys"));
    let t0 = Instant::now();
    assert_eq!(
        s.cpu_usage(t0),
        Some(20.0),
        "first sample: average since boot"
    );
    f.write("proc/stat", STAT_B);
    assert_eq!(
        s.cpu_usage(t0 + Duration::from_millis(200)),
        Some(20.0),
        "too soon: keeps the last value"
    );
    let u = s.cpu_usage(t0 + Duration::from_secs(1)).unwrap();
    assert!((u - 66.666).abs() < 0.01, "{u}");
    assert_eq!(s.cpu_ghz(), Some(2.5));
    assert!(
        Sampler::with_roots("/nonexistent", "/nonexistent")
            .cpu_usage(t0)
            .is_none()
    );
}

#[test]
fn sensors_from_hwmon_and_thermal() {
    let f = Fixture::new("temp");
    f.write("sys/class/hwmon/hwmon0/name", "macsmc_hwmon\n");
    f.write("sys/class/hwmon/hwmon0/temp1_input", "45500\n");
    f.write("sys/class/hwmon/hwmon0/temp1_label", "CPU\n");
    f.write("sys/class/hwmon/hwmon0/temp1_crit", "100000\n");
    f.write("sys/class/hwmon/hwmon0/temp2_input", "30000\n");
    f.write("sys/class/hwmon/hwmon0/fan1_input", "1200\n");
    f.write("sys/class/thermal/thermal_zone0/temp", "52000\n");
    f.write("sys/class/thermal/thermal_zone0/type", "acpitz\n");
    f.write("sys/class/thermal/cooling_device0/cur_state", "0\n");
    let s = sensors(&f.0.join("sys"));
    let names: Vec<&str> = s.iter().map(|x| x.name.as_str()).collect();
    assert_eq!(names, ["acpitz", "macsmc_hwmon/CPU", "macsmc_hwmon/temp2"]);
    let cpu = s.iter().find(|x| x.name.ends_with("/CPU")).unwrap();
    assert_eq!((cpu.celsius, cpu.critical), (45.5, Some(100.0)));
    assert!(sensors(Path::new("/nonexistent")).is_empty());
}

#[test]
fn disk_of_the_temp_dir() {
    let d = disk(&std::env::temp_dir()).unwrap();
    assert!(d.total > 0 && d.used() <= d.total);
    assert!((d.percent_used() + d.percent_free() - 100.0).abs() < 1e-9);
    assert!(disk(Path::new("/nonexistent/x")).is_none());
}

#[test]
fn net_dev_rates_and_sparkline() {
    let a = "Inter-|   Receive |  Transmit\n face |bytes packets errs drop fifo frame compressed multicast|bytes packets errs drop fifo colls carrier compressed\n    lo: 999 1 0 0 0 0 0 0 999 1 0 0 0 0 0 0\n wlan0: 1000 1 0 0 0 0 0 0 500 1 0 0 0 0 0 0\n veth1: 77 1 0 0 0 0 0 0 77 1 0 0 0 0 0 0\n";
    assert_eq!(
        parse_net_dev(a),
        Some((1000, 500)),
        "loopback and veth skipped"
    );
    assert!(parse_net_dev("nothing").is_none());
    assert_eq!(human_rate(512.0, false), "512B/s");
    assert_eq!(human_rate(1536.0, false), "1.5KB/s");
    assert_eq!(human_rate(300.0 * 1024.0, false), "300KB/s");
    assert_eq!(human_rate(1024.0 * 1024.0, true), "8.0Mb/s");
    assert_eq!(sparkline(&[0.0, 50.0, 100.0], Some(100.0)), "▁▅█");
    assert_eq!(sparkline(&[1.0, 2.0], None), "▅█");
    assert_eq!(sparkline(&[0.0, 0.0], None), "▁▁");
    assert_eq!(sparkline(&[], None), "");

    let dir = std::env::temp_dir().join(format!("sliver-net-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("net")).unwrap();
    let dev = |rx: u64, tx: u64| format!(" eth0: {rx} 1 0 0 0 0 0 0 {tx} 1 0 0 0 0 0 0\n");
    fs::write(dir.join("net/dev"), dev(1000, 1000)).unwrap();
    fs::write(dir.join("stat"), STAT_A).unwrap();
    let mut s = Sampler::with_roots(&dir, &dir);
    let t0 = Instant::now();
    assert_eq!(s.net_rates(t0), Some(NetRates::default()));
    fs::write(dir.join("net/dev"), dev(3000, 1500)).unwrap();
    assert_eq!(
        s.net_rates(t0 + Duration::from_millis(100)),
        Some(NetRates::default()),
        "too soon: unchanged"
    );
    let r = s.net_rates(t0 + Duration::from_secs(2)).unwrap();
    assert_eq!((r.down, r.up), (1000.0, 250.0));
    assert_eq!(s.net_history(), vec![1000.0]);
    s.cpu_usage(t0);
    assert_eq!(s.cpu_history().len(), 1);
    for i in 0..40u64 {
        fs::write(dir.join("net/dev"), dev(5000 + i, 0)).unwrap();
        s.net_rates(t0 + Duration::from_secs(3 + i));
    }
    assert_eq!(s.net_history().len(), HISTORY, "bounded");
    let _ = fs::remove_dir_all(&dir);
}

# Testing Sliver on an Intel T2 MacBook Pro

Thanks for helping! Sliver is a Touch Bar daemon plus customizer that replaces tiny-dfr. It works
on Apple Silicon. On T2 Macs every piece is covered by tests but **no one has run it on a real
T2 yet** (`docs/t2.md` explains what's different and what we're unsure about). This page takes
about 30 minutes, most of it waiting for the build.

**You can't lose your Touch Bar.** If sliverd fails, systemd hands the bar back to tiny-dfr by
itself. If it looks wrong, one command (below) brings tiny-dfr back. Uninstalling also gives the
bar back.

## You need

- A T2 MacBook Pro with a Touch Bar: MacBookPro15,1–15,4 (2170 px bar, no physical Esc) or
  16,1–16,4 (2008 px, physical Esc). Check with `cat /sys/class/dmi/id/product_name`.
- Fedora (a t2linux kernel, or Linux 6.15+ with `appletbdrm`, `hid-appletb-bl`, `hid-appletb-kbd`)
  and a Touch Bar that works under **tiny-dfr** today.
- Other distributions: skip to "No Fedora?" at the end.

## 1. Before installing (2 min, changes nothing)

```sh
git clone https://github.com/wawow830/sliver && cd sliver
cargo run -p sliver-hw --bin sliver-hwprobe -- --sysfs-only > hwprobe.txt
```

`hwprobe.txt` should name a `card` (the bar's display), a `bar backlight` (`appletb_backlight`, max
2) and a `touch` device (`Apple Inc. Touch Bar Display Touchpad`). It opens nothing, so it's safe
with tiny-dfr running. Keep the file for the report.

## 2. Build and install (≈15 min)

```sh
sudo dnf builddep packaging/rpm/sliver.spec
scripts/rpm-build --binary            # release build; RPMs land in target/rpmbuild/RPMS/
sudo dnf install target/rpmbuild/RPMS/*/*.rpm
```

Installing changes nothing yet: tiny-dfr keeps the bar. To switch now (or just reboot):

```sh
sudo systemctl start sliverd
systemctl --user start sliver-agent
```

## 3. What to check

Note what you see for each one. "Didn't work" is just as useful as "worked".

1. **The bar shows Sliver's layout**, sharp and the right way round: on a 2170 px bar, **Esc at the
   left**, then the controls on the right.
2. **Taps** do what the button says. With a terminal focused, try a few keys.
3. **Fn held** shows the F-keys (F1–F12, with Esc first on 2170 px bars). Release Fn and it goes
   back.
4. **Safe hatch:** press Fn twice quickly and keep holding. You should see the built-in F-keys
   whatever the layout is.
5. **Brightness:** the bar dims after 30 s without use and goes dark after 60 s. The first tap on a
   dark bar only wakes it, it doesn't press anything. The T2 bar has only off/dim/full, so "dim"
   may look almost the same as "on". Tell us how it looks.
6. **Suspend and resume**, and closing and opening the lid: does the bar come back?
7. **The settings app** (`sliver-settings`): move a button and watch the bar follow.

## 4. If anything goes wrong: back to tiny-dfr

```sh
sudo systemctl stop sliverd.socket sliverd
sudo systemctl start tiny-dfr
```

If sliverd crashes 5 times in a row, systemd does this by itself. To go back for good:
`sudo dnf remove sliverd sliver-agent` (the rest of Sliver goes with them). tiny-dfr gets the bar
right away.

## 5. What to send

Open an issue on <https://github.com/wawow830/sliver/issues> titled "T2 test: <your model>", with
your notes from step 3, `hwprobe.txt`, and the output of:

```sh
cat /sys/class/dmi/id/product_name; uname -r; rpm -q sliverd tiny-dfr
journalctl -b -u sliverd -u tiny-dfr --no-pager | tail -200
journalctl -b --user -u sliver-agent --no-pager | tail -100
# The bar's USB device: configuration 2 = display mode (Sliver's udev rule sets it)
for d in /sys/bus/usb/devices/*; do [ "$(cat $d/idProduct 2>/dev/null)" = 8302 ] && echo "$d config $(cat $d/bConfigurationValue)"; done
ls -l /sys/class/backlight/*/brightness
udevadm info /sys/class/drm/card* 2>/dev/null | grep -E '^P:|ID_SEAT|DRIVER|SYSTEMD_ALIAS'
sudo ausearch -m avc -ts boot 2>/dev/null | grep -i sliver
```

A photo of the bar, even a blurry one, helps a lot: we can't see your bar from the logs.

## No Fedora?

The RPM is Fedora-only. On other distributions you can still run the bounded hardware checks
from source with the safe-swap harness (it always gives the bar back to tiny-dfr). See "How to
help" in `docs/t2.md`.

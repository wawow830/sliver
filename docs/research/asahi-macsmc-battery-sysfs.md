# Asahi `macsmc-battery` sysfs interface

This note uses the Linux power-supply ABI at v6.12 and the owning Asahi Linux driver added in commit [`0ebf821cf6c75de2d95d3db277617ec685498e7c`](https://github.com/AsahiLinux/linux/commit/0ebf821cf6c75de2d95d3db277617ec685498e7c). The default contract is in [issue #1](https://github.com/wawow830/sliver/issues/1) and [issue #9](https://github.com/wawow830/sliver/issues/9).

## Sourced facts

### Identity and path

The power-supply ABI defines entries as `/sys/class/power_supply/<supply_name>/...`. Its `type` file reports the supply type, including `Battery`. ([Linux ABI, `Documentation/ABI/testing/sysfs-class-power`, v6.12](https://github.com/torvalds/linux/blob/v6.12/Documentation/ABI/testing/sysfs-class-power#L30-L37))

The Asahi driver describes the battery as `.name = "macsmc-battery"` and `.type = POWER_SUPPLY_TYPE_BATTERY`, then registers that descriptor when its battery probe succeeds. ([Asahi Linux, `drivers/power/supply/macsmc-power.c`, commit `0ebf821cf6c75de2d95d3db277617ec685498e7c`](https://github.com/AsahiLinux/linux/blob/0ebf821cf6c75de2d95d3db277617ec685498e7c/drivers/power/supply/macsmc-power.c#L485-L491), [registration](https://github.com/AsahiLinux/linux/blob/0ebf821cf6c75de2d95d3db277617ec685498e7c/drivers/power/supply/macsmc-power.c#L663-L680), [Kconfig support for Apple Silicon](https://github.com/AsahiLinux/linux/blob/0ebf821cf6c75de2d95d3db277617ec685498e7c/drivers/power/supply/Kconfig#L1135-L1144))

The Linux core puts power supplies in the `power_supply` class and sets the device name from `desc->name`. ([Linux, `drivers/power/supply/power_supply_core.c`, v6.12](https://github.com/torvalds/linux/blob/v6.12/drivers/power/supply/power_supply_core.c#L29-L39), [registration name](https://github.com/torvalds/linux/blob/v6.12/drivers/power/supply/power_supply_core.c#L1363-L1384)) Therefore the Asahi battery path is:

```text
/sys/class/power_supply/macsmc-battery
```

This is a driver-owned name, not a name shared by every Linux battery driver. The platform driver itself is named `macsmc-power`; that is a different name and is not the sysfs power-supply directory. ([Asahi driver, commit `0ebf821cf6c75de2d95d3db277617ec685498e7c`](https://github.com/AsahiLinux/linux/blob/0ebf821cf6c75de2d95d3db277617ec685498e7c/drivers/power/supply/macsmc-power.c#L836-L850))

### Files and wire format

The Asahi battery exposes the `STATUS` and `CAPACITY` power-supply properties. ([Asahi driver, commit `0ebf821cf6c75de2d95d3db277617ec685498e7c`](https://github.com/AsahiLinux/linux/blob/0ebf821cf6c75de2d95d3db277617ec685498e7c/drivers/power/supply/macsmc-power.c#L319-L342), [property list](https://github.com/AsahiLinux/linux/blob/0ebf821cf6c75de2d95d3db277617ec685498e7c/drivers/power/supply/macsmc-power.c#L673-L680)) Read these files:

```text
/sys/class/power_supply/macsmc-battery/capacity
/sys/class/power_supply/macsmc-battery/status
```

- `capacity` is an integer percentage. The ABI's valid range is `0` through `100`, and the class documentation defines `CAPACITY` in percent. ([Linux ABI, v6.12](https://github.com/torvalds/linux/blob/v6.12/Documentation/ABI/testing/sysfs-class-power#L261-L271), [class documentation, v6.12](https://github.com/torvalds/linux/blob/v6.12/Documentation/power/power_supply_class.rst#L60-L72)) The Asahi getter passes the SMC `BUIC` byte through as the capacity value, so userspace should enforce the ABI range rather than assume that the driver clamps it. ([Asahi driver, commit `0ebf821cf6c75de2d95d3db277617ec685498e7c`](https://github.com/AsahiLinux/linux/blob/0ebf821cf6c75de2d95d3db277617ec685498e7c/drivers/power/supply/macsmc-power.c#L339-L342))
- `status` is one of the exact strings `Unknown`, `Charging`, `Discharging`, `Not charging`, or `Full`. ([Linux ABI, v6.12](https://github.com/torvalds/linux/blob/v6.12/Documentation/ABI/testing/sysfs-class-power#L468-L480), [Linux status text table, v6.12](https://github.com/torvalds/linux/blob/v6.12/drivers/power/supply/power_supply_sysfs.c#L76-L82)) The Asahi driver returns the corresponding Linux status enums, including distinct `Full`, `Not charging`, and `Charging` results. ([Asahi driver, commit `0ebf821cf6c75de2d95d3db277617ec685498e7c`](https://github.com/AsahiLinux/linux/blob/0ebf821cf6c75de2d95d3db277617ec685498e7c/drivers/power/supply/macsmc-power.c#L141-L190))
- Sysfs adds one newline to both forms: integer properties use `"%d\n"`, and enum text properties use `"%s\n"`. ([Linux, `drivers/power/supply/power_supply_sysfs.c`, v6.12](https://github.com/torvalds/linux/blob/v6.12/drivers/power/supply/power_supply_sysfs.c#L280-L318)) A normal read therefore returns, for example, `"87\n"` or `"Charging\n"`, with no percent sign or other unit suffix. This is also why `type`, if checked, reads as `"Battery\n"` for this descriptor. ([Linux type text table, v6.12](https://github.com/torvalds/linux/blob/v6.12/drivers/power/supply/power_supply_sysfs.c#L47-L61))

A driver property read can fail rather than return text. The Asahi getter propagates a negative status or capacity read result, and the power-supply sysfs layer propagates property errors to the file reader. ([Asahi driver, commit `0ebf821cf6c75de2d95d3db277617ec685498e7c`](https://github.com/AsahiLinux/linux/blob/0ebf821cf6c75de2d95d3db277617ec685498e7c/drivers/power/supply/macsmc-power.c#L319-L342), [Linux sysfs error handling, v6.12](https://github.com/torvalds/linux/blob/v6.12/drivers/power/supply/power_supply_sysfs.c#L280-L295))

## Implementation recommendations for the default

1. Hard-code `/sys/class/power_supply/macsmc-battery` for this Asahi-only default. The driver explicitly supplies that power-supply name. ([Asahi descriptor](https://github.com/AsahiLinux/linux/blob/0ebf821cf6c75de2d95d3db277617ec685498e7c/drivers/power/supply/macsmc-power.c#L485-L491)) Enumerating the directory and choosing the first `Battery` would make an arbitrary provider choice. If defensive verification is wanted, compare the directory basename with `macsmc-battery` and optionally require `type` to equal `Battery` after removing its one final newline. The ABI identifies the basename as `<supply_name>` and documents `type` as the standard discriminator, so do not make a separate `/name` property a dependency. ([Linux ABI](https://github.com/torvalds/linux/blob/v6.12/Documentation/ABI/testing/sysfs-class-power#L30-L37), [Linux core naming](https://github.com/torvalds/linux/blob/v6.12/drivers/power/supply/power_supply_core.c#L1363-L1384)) If support later expands to other drivers, add an explicit fallback rather than silently choosing the first battery.
2. Use ordinary Lua file access: `io.open(path, "r")`, `file:read("*a")`, and `file:close()`. The issue requires the embedded default to use ordinary Lua and no external commands. ([issue #1](https://github.com/wawow830/sliver/issues/1))
3. Parse `capacity` strictly as decimal digits followed by the expected final `\n`. The kernel formatter emits that integer newline form. ([Linux sysfs formatter](https://github.com/torvalds/linux/blob/v6.12/drivers/power/supply/power_supply_sysfs.c#L312-L318)) Convert it with `tonumber` and accept only `0 <= value <= 100`. Reject an empty string, extra lines or whitespace, a sign, a decimal or unit suffix, and every out-of-range value.
4. Parse `status` against the exact five-value whitelist after removing exactly one final `\n`. Do not lowercase or normalize `Not charging`. ([Linux status ABI](https://github.com/torvalds/linux/blob/v6.12/Documentation/ABI/testing/sysfs-class-power#L468-L480))
5. For the issue's charging color, set `charging = true` only for the exact status `Charging`. Treat `Full`, `Not charging`, `Discharging`, and `Unknown` as not charging. The kernel driver deliberately reports `Full` and `Not charging` as separate states, and the issue asks for green while charging, not merely while an adapter is present. ([Asahi status logic](https://github.com/AsahiLinux/linux/blob/0ebf821cf6c75de2d95d3db277617ec685498e7c/drivers/power/supply/macsmc-power.c#L141-L190), [issue #1](https://github.com/wawow830/sliver/issues/1))
6. Treat each field independently. A missing, unreadable, malformed, or out-of-range `capacity` makes the capacity unavailable. Render `--%` in that case, as required by the issue. ([issue #1](https://github.com/wawow830/sliver/issues/1)) A missing, unreadable, or malformed `status` makes only the charging state unknown, so retain a valid percentage but use no green color. Never substitute `0`, `100`, or `Charging`, and never use the charging color for an unavailable capacity.

The result should be a small record such as `{ capacity = number_or_nil, status = status_or_nil, charging = status == "Charging" }`. No shell, command lookup, `cat`, or distro-specific battery path is needed.

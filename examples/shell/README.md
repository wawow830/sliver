# Shell examples

`sliverctl` is the easy path. `busctl` works too, but a context lives only as long as the bus
connection that created it, so one-shot `busctl call`s are only good for poking at things.

## A button that runs something (sliverctl)
```sh
sliverctl publish --provider org.example.Build button:build:'🔨 Build' |
  while read -r item kind _; do
    [ "$kind" = tap ] && make -C ~/src/project && notify-send "build ok"
  done
```

## Live label from a script (`--stdin` takes `ID KEY=VALUE…` lines)
```sh
{
  while sleep 2; do
    echo "load label='load $(cut -d' ' -f1 /proc/loadavg)'"
  done
} | sliverctl publish --provider org.example.Load --stdin label:load:load
```

## Two-way with coproc (bash)
```sh
coproc BAR { sliverctl publish --provider org.example.Mic --stdin toggle:mic:'🎤'; }
while read -r item kind state; do               # state is e.g. "state=true"
  [ "$state" = state=true ] && wpctl set-mute @DEFAULT_SOURCE@ 1 || wpctl set-mute @DEFAULT_SOURCE@ 0
done <&"${BAR[0]}" &
echo "mic label='🔇'" >&"${BAR[1]}"          # update it any time
```

## Controls for another app (BetterTouchTool-style)
```sh
sliverctl publish --scope target --target-app firefox --provider org.example.FxTabs \
  button:back:◀ button:fwd:▶ | while read -r item kind _; do
    case "$item" in back) wtype -M alt -k Left ;; fwd) wtype -M alt -k Right ;; esac
  done
```

## waybar-style control
```sh
sliverctl signal 8                  # like pkill -RTMIN+8 waybar: re-run modules with signal = 8
sliverctl update custom/status text='deploy ✓' class=ok
sliverctl toggle 'app/org.example.Build#build'
sliverctl layer push media 5000     # media layer for 5 s
sliverctl bar hide                  # Fn Fn-hold still brings back the F-keys
```

## Raw D-Bus (busctl)
```sh
busctl --user introspect io.github.wawow830.Sliver /io/github/wawow830/Sliver
busctl --user get-property io.github.wawow830.Sliver /io/github/wawow830/Sliver \
  io.github.wawow830.Sliver.Control1 ActiveLayer
busctl --user call io.github.wawow830.Sliver /io/github/wawow830/Sliver \
  io.github.wawow830.Sliver.Control1 Signal u 8
```

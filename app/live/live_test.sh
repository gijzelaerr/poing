#!/bin/bash
# Live mode without the hardware: drives a running Poing with the OMNIS-DUO's
# MIDI messages (makepad-remote `midi`, injected as port OMNIS-DUO) and reads
# the decks back with the `live` tool. docs/LIVE.md has the numbers of a run.
#
# Start a hidden, muted Poing on a copy of a song, with its own config folder
# (PowerShell; the PID it prints is this script's argument):
#   $env:APPDATA = "<an empty folder>"; $env:POING_MUTE = "1"; $env:MAKEPAD_HIDE_WINDOWS = "1"
#   (Start-Process target\release\poing.exe -ArgumentList "--remote", "<copy>\its-autumn.poing\project.json" -PassThru).Id
# then:  app/live/live_test.sh <pid>
# makepad-remote: MAKEPAD_REMOTE, else on PATH (cargo install --git
# https://github.com/gijzelaerr/makepad.git --tag <the pin in Cargo.toml> makepad-remote).
R=${MAKEPAD_REMOTE:-makepad-remote}
P=$1
[ -n "$P" ] || { echo "usage: $0 <poing pid>"; exit 2; }
FAILS=0
m() { $R app $P midi a=$1 b=$2 c=$3 p=OMNIS-DUO >/dev/null; }
press() { m $1 $2 127; m $1 $2 0; }
# live [json] [python check on d (all), a (deck 1), b (deck 2)]
live() {
  $R app $P call poing.live "${1:-{\}}" | python -c "
import sys,json; r=json.load(sys.stdin)['Result'][0]; d=json.loads(r['data'])
print('  bpm',d['bpm'],'phase_diff',d['phase_diff_beats'],'xf',d['xfader'],'fx',d['fx'])
for k in d['decks']:
    print('   ', {x:k.get(x) for x in ['playing','t','beat','speed','bpm','sync','section','loop','level_db']}, 'stems', ''.join('1' if v else '0' for v in k['stems_playing'].values()))
a,b=d['decks']
ok=eval(sys.argv[1]) if len(sys.argv)>1 else True
print('  CHECK', 'ok' if ok else 'FAILED', sys.argv[1] if len(sys.argv)>1 else '')
sys.exit(0 if ok else 1)" "$2" || FAILS=$((FAILS+1))
}
out() { $R app $P midi k=out | python -c "
import sys,json; d=json.load(sys.stdin); m=[tuple(x['d']) for x in d['m']]
print('  LEDs sent:', d['n'], ' '.join('%02X%02X%02X'%x for x in m[-24:]))
want=[tuple(int(w[i:i+2],16) for i in (0,2,4)) for w in sys.argv[1:]]
missing=[w for w in want if w not in m]
print('  CHECK', 'ok' if not missing else 'FAILED, not sent: %s'%missing)
sys.exit(1 if missing else 0)" "$@" || FAILS=$((FAILS+1))
}

$R app $P midi k=reset >/dev/null
echo "== live on: both decks get the song"
live '{"on":true}' "a['loaded'] and b['loaded'] and a['hot'][0] is not None"
sleep 0.5
echo "== deck 2 tempo +3% (B1 00 57, B1 20 7F), deck 1 play, deck 2 hot cue B"
m 177 0 87; m 177 32 127
press 144 0
press 150 1
sleep 3; live '' "a['playing'] and b['playing'] and abs(b['speed']-1.03)<0.002 and abs(b['t']-a['hot'][1]['t']-3*1.03)<0.5"
echo "== deck 2 sync (91 1F); 3 s later in phase"
press 145 31
sleep 3; live '' "b['sync'] and abs(d['phase_diff_beats'])<0.02"
echo "== deck 1 hot cue C (95 02) while playing: the phase is kept"
press 149 2
sleep 1; live '' "a['section'].startswith('Chorus') and a['t']>a['hot'][2]['t'] and abs(d['phase_diff_beats'])<0.02"
echo "== shift + pads: deck 1 vocals and drums off (95 08, 95 09), deck 2 bass (96 0A)"
press 149 8; press 149 9; press 150 10
sleep 0.3; live '' "list(a['stems_playing'].values())==[False,False,True,True] and list(b['stems_playing'].values())==[True,True,False,True]"
out 950800 950900 960A00 90007F 91007F
echo "== crossfader hard left (B4 0B 00): deck 2 silent"
m 180 11 0; sleep 0.5; live '' "b['level_db']=='-inf' or float(b['level_db'])<-40"
echo "== crossfader hard right (B4 0B 7F): deck 1 silent"
m 180 11 127; sleep 0.5; live '' "a['level_db']=='-inf' or float(a['level_db'])<-40"
m 180 11 64
echo "== deck 1 low kill (B4 04 00), filter down (B4 05 20); deck 2 loop on (91 43), halved (B1 2E 3F)"
m 180 4 0; m 180 5 32; press 145 67; m 177 46 63
sleep 2; live '' "a['eq']['low']==0 and b['loop']=={'on':True,'beats':2.0} and abs(d['phase_diff_beats'])<0.02"
echo "== beat FX on (B4 72), echo 1 beat (B4 4D)"
press 180 114; press 180 77
sleep 0.5; live '' "d['fx']['on'] and d['fx']['beats']==1.0"
echo "== deck 1 jog: touched, 72 steps back: it goes back and holds; deck 2 keeps its tempo"
m 144 32 127; for i in 1 2 3 4 5 6 7 8; do m 176 34 55; done
sleep 0.5; live '' "a['speed']==0 and b['playing'] and 0.9<b['speed']<1.1"
m 144 32 0
echo "== an unknown message (96 0C 7F) is logged; both decks stop"
press 150 12; press 144 0; press 145 0
sleep 0.5; live '' "not a['playing'] and not b['playing']"
echo "== live off"
live '{"on":false}' "not d['on']"
echo "$FAILS check(s) failed"
exit $FAILS

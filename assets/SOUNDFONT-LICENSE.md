# Embedded soundfont

`TimGM6mb.sf2` — a 6 MB General MIDI soundfont by **Tim Brechbill**,
distributed with MuseScore and TiMidity++.

- License: **GNU General Public License v2.0**
- Source: distributed via the MuseScore project and mirrored in
  [pretty_midi](https://github.com/craffel/pretty-midi)

This is the soundfont the spec suggests for the embedded default
(spec §8, open question 4). Note the licensing consequence: binaries
built with the default embedded soundfont include GPL-2.0 data. If
that matters for your distribution, build after replacing
`assets/TimGM6mb.sf2` with a differently-licensed SF2, or ship MIDI
only — `--soundfont` swaps the SF2 at runtime without rebuilding.

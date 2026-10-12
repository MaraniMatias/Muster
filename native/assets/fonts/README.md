# Terminal fonts

Fira Code Regular and Bold are the unhinted faces bundled in
`legacy/src/assets/fonts/`, decompressed to TrueType for GPUI's native font APIs.
The SIL Open Font License is included in `LICENSE-FiraCode.txt`.

To regenerate with fontTools (including WOFF2 support) installed:

```sh
python3 - <<'PY'
from fontTools.ttLib import TTFont
for weight in ('Regular', 'Bold'):
    font = TTFont(f'legacy/src/assets/fonts/FiraCode-{weight}.woff2')
    font.flavor = None
    font.save(f'native/assets/fonts/FiraCode-{weight}.ttf')
PY
```

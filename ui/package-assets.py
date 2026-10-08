"""Package the assets from the resolved published rustradio-ui dependency."""
import json
from pathlib import Path
import shutil

root = Path(__file__).resolve().parent
dist = root / "web-dist"
metadata = json.loads((dist / "metadata.json").read_text())
package = next(p for p in metadata["packages"] if p["name"] == "rustradio-ui")
if not str(package["source"]).startswith("registry+"):
    raise SystemExit("rustradio-ui must be a published registry dependency")
assets = Path(package["manifest_path"]).parent / "assets"
shutil.copyfile(assets / "bootstrap.js", dist / "rustradio-ui-bootstrap.js")
for name in ("index.html", "wasm-mod.js", "theme.js"):
    shutil.copyfile(root / "web" / name, dist / name)
(dist / "style.css").write_text(
    (assets / "rustradio.css").read_text() + "\n" + (root / "web/style.css").read_text()
)

// Extrahiert Lucide-Icon-Geometrie aus dem npm-Paket lucide-react und
// generiert src/ui/icons_data.rs für den raylib-Icon-Renderer.
//
// Die generierte icons_data.rs ist eingecheckt — dieses Skript ist nur
// nötig, wenn neue Icons in NAMES aufgenommen werden:
//   npm i --prefix /tmp/lucide lucide-react
//   node tools/extract_icons.mjs /tmp/lucide/node_modules/lucide-react
import { writeFileSync } from "node:fs";
import { resolve } from "node:path";

const lucideRoot = process.argv[2];
if (!lucideRoot) {
  console.error("Aufruf: node tools/extract_icons.mjs <pfad-zu-lucide-react>");
  process.exit(1);
}

const NAMES = [
  "activity","arrow-right-from-line","arrow-right-to-line","audio-lines","audio-waveform",
  "ban","blend","check","chevrons-left-right","arrow-left-right","clipboard-paste","chevron-down","chevron-left","chevron-right","chevron-up","circle-alert",
  "circle-check","clapperboard","clock","copy","crop","droplets","eye","eye-off","file-output",
  "film","focus","folder-open","folder-search","gauge","grip-vertical","hand","headphones",
  "history","image","import","info","keyboard","layout-grid","link-2","list","list-video",
  "loader-circle","lock","lock-open","magnet","maximize-2","mic","minus","monitor-off",
  "monitor-play","mouse-pointer","move","move-horizontal","music","palette","panels-top-left",
  "pause","play","plus","repeat","rotate-ccw","scissors","search","skip-back","skip-forward",
  "sliders-horizontal","sparkles","square-terminal","step-back","step-forward",
  "stretch-horizontal","timer","trash-2","triangle-alert","type","volume-2","x","zoom-in","zoom-out",
];

function rustStr(s) {
  return JSON.stringify(s);
}

const entries = [];
for (const name of NAMES) {
  const mod = await import(resolve(lucideRoot, `dist/esm/icons/${name}.mjs`));
  const node = mod.__iconNode;
  if (!node) {
    console.error(`!! ${name}: kein __iconNode`);
    continue;
  }
  const els = node.map(([tag, attrs]) => {
    switch (tag) {
      case "path":
        return `E::Path(${rustStr(attrs.d)})`;
      case "circle":
        return `E::Circle(${Number(attrs.cx)}f32, ${Number(attrs.cy)}f32, ${Number(attrs.r)}f32)`;
      case "ellipse":
        return `E::Ellipse(${Number(attrs.cx)}f32, ${Number(attrs.cy)}f32, ${Number(attrs.rx)}f32, ${Number(attrs.ry)}f32)`;
      case "line":
        return `E::Line(${Number(attrs.x1)}f32, ${Number(attrs.y1)}f32, ${Number(attrs.x2)}f32, ${Number(attrs.y2)}f32)`;
      case "rect": {
        const rx = attrs.rx !== undefined ? Number(attrs.rx) : 0;
        return `E::Rect(${Number(attrs.x)}f32, ${Number(attrs.y)}f32, ${Number(attrs.width)}f32, ${Number(attrs.height)}f32, ${rx}f32)`;
      }
      case "polyline":
        return `E::Polyline(${rustStr(attrs.points)})`;
      case "polygon":
        return `E::Polygon(${rustStr(attrs.points)})`;
      default:
        throw new Error(`${name}: unbekanntes Element <${tag}>`);
    }
  });
  entries.push(`    (${rustStr(name)}, &[${els.join(", ")}]),`);
}

const out = `// Generiert aus lucide-react (ISC-Lizenz) — nicht von Hand editieren.
// Quelle: dist/esm/icons/*.mjs, extrahiert via tools/extract_icons.mjs
use super::icons::IconElement as E;

pub static ICON_DATA: &[(&str, &[E])] = &[
${entries.join("\n")}
];
`;
writeFileSync(new URL("../src/ui/icons_data.rs", import.meta.url), out);
console.log(`OK: ${entries.length} Icons -> src/ui/icons_data.rs`);

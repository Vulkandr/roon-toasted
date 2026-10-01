// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Writes the updater's `latest.json` for a release, next to the installer.
//
// Run after `npm run tauri build` (with the signing key in the environment,
// see README "Releasing"). Reads the version from tauri.conf.json, the
// installer's signature from the `.sig` the build made, and the release
// notes from RELEASE_NOTES.md (or the file given as the first argument).
// Upload the three files it names to the GitHub release `v<version>`.
//
//   node scripts/latest-json.mjs [notes.md]

import fs from "node:fs";
import path from "node:path";

const root = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..");
const conf = JSON.parse(fs.readFileSync(path.join(root, "src-tauri", "tauri.conf.json"), "utf8"));
const version = conf.version;
const product = conf.productName; // "Roon Toasted"
const bundleDir = path.join(root, "src-tauri", "target", "release", "bundle", "nsis");
const installer = path.join(bundleDir, `${product}_${version}_x64-setup.exe`);
const signature = `${installer}.sig`;
const notesFile = path.resolve(process.argv[2] ?? path.join(root, "RELEASE_NOTES.md"));

for (const [what, file] of [["installer", installer], ["signature", signature], ["release notes", notesFile]]) {
	if (!fs.existsSync(file)) {
		console.error(`${what} not found: ${file}`);
		process.exit(1);
	}
}

// GitHub replaces spaces in asset names with dots.
const assetName = path.basename(installer).replace(/ /g, ".");
const latest = {
	version,
	notes: fs.readFileSync(notesFile, "utf8").trim(),
	pub_date: new Date().toISOString(),
	platforms: {
		"windows-x86_64": {
			signature: fs.readFileSync(signature, "utf8").trim(),
			url: `https://github.com/Vulkandr/roon-toasted/releases/download/v${version}/${assetName}`,
		},
	},
};

const out = path.join(bundleDir, "latest.json");
fs.writeFileSync(out, JSON.stringify(latest, null, 2) + "\n");
console.log(`wrote ${out}`);
console.log("Upload these to the GitHub release v" + version + ":");
for (const f of [installer, signature, out]) console.log("  " + f);

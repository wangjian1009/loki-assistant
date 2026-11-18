import { cp, mkdir, readdir, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { stripTypeScriptTypes } from "node:module";

const root = fileURLToPath(new URL("..", import.meta.url));
const extension = join(root, "extension");
const dist = fileURLToPath(new URL("../../dist/chrome-plugin/", import.meta.url));
const source = join(extension, "features", "org-capture");
const output = join(dist, "features", "org-capture");

await rm(dist, { recursive: true, force: true });
await mkdir(join(output, "html2org"), { recursive: true });

for (const name of await readdir(join(source, "html2org"))) {
  if (!name.endsWith(".ts")) continue;
  const input = await readFile(join(source, "html2org", name), "utf8");
  const javascript = stripTypeScriptTypes(input).replace(
    /(from\s+["'])(\.\/[\w-]+)(["'])/g,
    "$1$2.js$3",
  );
  await writeFile(join(output, "html2org", name.replace(/\.ts$/, ".js")), javascript);
}

const capture = await readFile(join(source, "capture.js"), "utf8");
await writeFile(
  join(output, "capture.js"),
  capture.replaceAll(/\.ts(["'])/g, ".js$1"),
);

for (const name of ["manifest.json", "popup", "background", "shared", "icons"]) {
  const input = join(extension, name);
  const target = join(dist, name);
  await mkdir(dirname(target), { recursive: true });
  await cp(input, target, { recursive: true });
}

console.log("构建完成：dist/chrome-plugin/");

import { cp, mkdir, readdir, readFile, rm, stat, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { stripTypeScriptTypes } from "node:module";

const root = fileURLToPath(new URL("..", import.meta.url));
const extension = join(root, "extension");
const dist = fileURLToPath(new URL("../../dist/chrome-plugin/", import.meta.url));
await rm(dist, { recursive: true, force: true });

function rewriteImports(source) {
  return source.replace(
    /(\b(?:from|import\s*\()\s*["'])(\.\.?\/[^"']+?)(["'])/g,
    (_match, prefix, specifier, suffix) => {
      if (specifier.endsWith(".ts")) {
        return `${prefix}${specifier.slice(0, -3)}.js${suffix}`;
      }
      if (/\.(?:js|json|css|html|png|svg)$/u.test(specifier)) {
        return `${prefix}${specifier}${suffix}`;
      }
      return `${prefix}${specifier}.js${suffix}`;
    },
  );
}

async function copySourceTree(sourceDir, outputDir) {
  await mkdir(outputDir, { recursive: true });
  for (const name of await readdir(sourceDir)) {
    const sourcePath = join(sourceDir, name);
    const outputPath = join(outputDir, name.replace(/\.ts$/u, ".js"));
    const sourceStat = await stat(sourcePath);
    if (sourceStat.isDirectory()) {
      await copySourceTree(sourcePath, outputPath);
      continue;
    }
    if (name.endsWith(".ts")) {
      await writeFile(outputPath, rewriteImports(stripTypeScriptTypes(await readFile(sourcePath, "utf8"))));
    } else if (name.endsWith(".js")) {
      await writeFile(outputPath, rewriteImports(await readFile(sourcePath, "utf8")));
    } else {
      await cp(sourcePath, outputPath);
    }
  }
}

await copySourceTree(join(extension, "features"), join(dist, "features"));

for (const name of ["manifest.json", "popup", "background", "shared", "icons"]) {
  const input = join(extension, name);
  const target = join(dist, name);
  await mkdir(dirname(target), { recursive: true });
  await cp(input, target, { recursive: true });
}

console.log("构建完成：dist/chrome-plugin/");

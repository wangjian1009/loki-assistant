const fs = require('node:fs');
const [metadata, models, output] = process.argv.slice(2);
const entries = fs.readFileSync(metadata, 'utf8').trim().split('\n').map(line => JSON.parse(line));
const implementations = new Map(entries.filter(item => item.kind === 'impl').map(item => [item.name, item.def]));
const declarations = entries.flatMap(({ kind, name, def }) => {
  if (kind === 'impl') return [];
  if (kind === 'fn') return [def];
  if (kind !== 'struct' && kind !== 'interface') throw new Error(`未知 NAPI 声明类型 ${kind}`);
  const body = kind === 'struct' ? implementations.get(name) ?? '' : def;
  const lines = body.split('\\n').map(line => line.trim()).filter(Boolean);
  if (kind === 'struct') lines.unshift('private constructor()');
  return [`export ${kind === 'struct' ? 'class' : kind} ${name} {\n${lines.map(line => `  ${line}`).join('\n')}\n}`];
});
const contents = '// 原生方法由 Rust NAPI 宏生成。\n' + fs.readFileSync(models, 'utf8') +
  '\n' + declarations.join('\n\n') + '\n';
if (!fs.existsSync(output) || fs.readFileSync(output, 'utf8') !== contents) fs.writeFileSync(output, contents);

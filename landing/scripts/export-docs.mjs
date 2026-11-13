// Export the website's authored content without running its browser code.
// Uses the Svelte parser already installed for the landing build; no extra deps.
import { readFileSync, writeFileSync, mkdirSync, readdirSync, existsSync } from 'node:fs';
import { dirname, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parse } from 'svelte/compiler';

const root = fileURLToPath(new URL('../../', import.meta.url));
const check = process.argv.includes('--check');
const outputs = new Map();
const read = (path) => readFileSync(resolve(root, path), 'utf8');
const load = (path) => parse(read(path), { modern: true });
const docs = load('landing/src/Docs.svelte');
const landing = load('landing/src/Landing.svelte');

// Only literal data is evaluated. Unknown expressions fail the export instead
// of silently dropping newly added content. Icons are presentational only.
function value(node, scope) {
  switch (node.type) {
    case 'Literal': return node.value;
    case 'TemplateLiteral':
      return node.quasis.map((q, i) => q.value.cooked +
        (node.expressions[i] ? value(node.expressions[i], scope) : '')).join('');
    case 'Identifier':
      if (node.name in scope) return scope[node.name];
      throw new Error(`Unknown content variable: ${node.name}`);
    case 'ArrayExpression': return node.elements.map((n) => value(n, scope));
    case 'ObjectExpression': return Object.fromEntries(node.properties
      .filter((p) => p.key.name !== 'icon')
      .map((p) => [p.key.name ?? p.key.value, value(p.value, scope)]));
    case 'MemberExpression':
      return value(node.object, scope)[node.computed ? value(node.property, scope) : node.property.name];
    case 'CallExpression':
      // Animated headings resolve to their final, authored string.
      if (node.callee.type === 'Identifier' && node.arguments.length === 0)
        return value(node.callee, scope);
      break;
  }
  throw new Error(`Unsupported content expression: ${node.type}`);
}

function constants(ast) {
  const scope = {};
  for (const statement of ast.instance.content.body) {
    if (statement.type !== 'VariableDeclaration' || statement.kind !== 'const') continue;
    for (const { id, init } of statement.declarations) {
      if (/^(nav|tutorial|terminalLines|finalTitle|installCmd|c[A-Z]\w*|code\w+)$/.test(id.name))
        scope[id.name] = value(init, scope);
      if (init?.type === 'CallExpression' && init.callee.name === 'useScramble')
        scope[id.name] = value(init.arguments[0], scope);
    }
  }
  return scope;
}
const docData = constants(docs);
const homeData = constants(landing);
const topics = docData.nav.flatMap((group) => group.items);
const topicIds = new Set(topics.map((topic) => topic.id));

function findAll(node, predicate) {
  if (!node || typeof node !== 'object') return [];
  const matches = predicate(node) ? [node] : [];
  // Traverse markup, never script expressions or attributes.
  for (const key of ['fragment', 'nodes', 'body', 'consequent', 'alternate']) {
    const children = node[key];
    for (const child of Array.isArray(children) ? children : [children])
      matches.push(...findAll(child, predicate));
  }
  return matches;
}
function attr(node, name, scope = {}) {
  const a = node.attributes?.find((a) => a.type === 'Attribute' && a.name === name);
  if (!a || a.value === true) return '';
  return a.value.map((n) => n.type === 'Text' ? n.data : value(n.expression, scope)).join('');
}
const elements = (node, name) => findAll(node, (n) => n.type === 'RegularElement' && n.name === name);
const articles = elements(docs.fragment, 'article');
const articleIds = new Set(articles.map((a) => attr(a, 'id')));
if (articles.length !== topics.length || articleIds.size !== topics.length ||
    topics.some((topic) => !articleIds.has(topic.id)))
  throw new Error('Reference articles and sidebar navigation do not match');

function link(href, file) {
  let target;
  if (href.startsWith('#/docs/')) {
    const id = href.slice('#/docs/'.length);
    if (!topicIds.has(id)) throw new Error(`Unknown docs link: ${href}`);
    target = `docs/reference/${id}.md`;
  } else if (href === '#/docs') target = 'docs/README.md';
  else if (href === '#/' || href === '#') target = 'docs/OVERVIEW.md';
  else if (href === '#docs') target = 'docs/TUTORIAL.md';
  else if (href.startsWith('#ch-')) target = `docs/TUTORIAL.md#${href.slice(4)}`;
  else if (href.startsWith('#')) target = `docs/OVERVIEW.md${href}`;
  else {
    const repo = href.match(/^https:\/\/github\.com\/enekos\/sutegi\/(?:blob|tree)\/master\/(.+)$/);
    if (repo) target = repo[1];
    else if (href === 'https://github.com/enekos/sutegi#readme') target = 'README.md';
    else return href;
  }
  const [path, anchor] = target.split('#');
  return relative(dirname(file), path) + (anchor ? `#${anchor}` : '');
}

const clean = (text) => text.replace(/[ \t]+\n/g, '\n').trim();
const escape = (text) => text.replace(/([\\`*_[\]<>])/g, '\\$1');
function inlineCode(text) {
  const ticks = '`'.repeat(Math.max(0, ...(text.match(/`+/g) ?? []).map((s) => s.length)) + 1);
  const pad = text.startsWith('`') || text.endsWith('`') ? ' ' : '';
  return ticks + pad + text + pad + ticks;
}
function fence(text, language = 'rust') {
  const ticks = '`'.repeat(Math.max(2, ...(text.match(/`+/g) ?? []).map((s) => s.length)) + 1);
  return `\n\n${ticks}${language}\n${text}\n${ticks}\n\n`;
}
const languages = {
  cInstall: 'text', cFirstRun: 'bash', cEnv: 'dotenv', cCli: 'bash',
  cMigrateCli: 'bash', cAgents: 'bash', cChannelsWire: 'jsonc',
  cChannelsJs: 'javascript', cRepl: 'text', cInternals: 'text', cDeploy: 'bash',
};

function render(node, scope, file, plain = false) {
  if (!node) return '';
  const children = () => (node.fragment?.nodes ?? node.nodes ?? [])
    .map((n) => render(n, scope, file, plain)).join('');
  switch (node.type) {
    case 'Fragment': return children();
    case 'Text': {
      const text = node.data.replace(/\s+/g, ' ');
      return plain ? text : escape(text);
    }
    case 'ExpressionTag': {
      const text = String(value(node.expression, scope));
      return plain ? text : escape(text);
    }
    case 'Comment': case 'Component': case 'ConstTag': return '';
    case 'EachBlock':
      return value(node.expression, scope).map((item, i) => render(node.body,
        { ...scope, [node.context.name]: item, [node.index ?? '__index']: i }, file, plain)).join('');
    case 'RenderTag': {
      if (node.expression.callee.name !== 'code') throw new Error('Unknown snippet');
      const arg = node.expression.arguments[0];
      return fence(value(arg, scope), languages[arg.name] ?? 'rust');
    }
    case 'RegularElement': break;
    default: throw new Error(`Unsupported markup: ${node.type}`);
  }
  const tag = node.name;
  if (['button', 'input', 'aside', 'nav'].includes(tag)) return '';
  if (tag === 'code') return inlineCode(render(node.fragment, scope, file, true).trim());
  if (tag === 'pre') {
    const text = node.fragment.nodes.map((n) => n.type === 'ExpressionTag'
      ? value(n.expression, scope) : n.type === 'Text' ? n.data : render(n, scope, file, true)).join('');
    return fence(text.trim(), 'text');
  }
  if (tag === 'table') {
    const rows = elements(node, 'tr').map((tr) => tr.fragment.nodes
      .filter((n) => n.type === 'RegularElement' && ['th', 'td'].includes(n.name))
      .map((cell) => clean(render(cell.fragment, scope, file)).replace(/\|/g, '\\|').replace(/\n+/g, '<br>')));
    const row = (cells) => `| ${cells.join(' | ')} |`;
    return `\n\n${[row(rows[0]), row(rows[0].map(() => '---')), ...rows.slice(1).map(row)].join('\n')}\n\n`;
  }
  const text = children();
  if (plain) return text;
  if (/^h[1-6]$/.test(tag)) return `\n\n${'#'.repeat(Math.max(1, Number(tag[1]) - (file.includes('/reference/') ? 1 : 0)))} ${text.trim()}\n\n`;
  if (tag === 'a') return `[${text.trim().replace(/\s+/g, ' ')}](${link(attr(node, 'href', scope), file)})`;
  if (tag === 'strong') return `**${text.trim()}**`;
  if (tag === 'em') return `*${text.trim()}*`;
  if (tag === 'br') return ' ';
  if (tag === 'li') return `- ${text.trim()}\n`;
  if (['p', 'ul', 'ol'].includes(tag)) return `\n\n${text.trim()}\n\n`;
  if (tag === 'div' && attr(node, 'class').includes('callout-title')) return `**${text.trim()}**\n\n`;
  if (tag === 'div' && attr(node, 'class').split(' ').includes('callout')) {
    const kind = attr(node, 'class').includes('warn') ? 'WARNING' : attr(node, 'class').includes('tip') ? 'TIP' : 'NOTE';
    return `\n\n> [!${kind}]\n${clean(text).replace(/\n{3,}/g, '\n\n').split('\n').map((line) => `> ${line}`).join('\n')}\n\n`;
  }
  if (['div', 'section', 'article', 'main', 'footer'].includes(tag)) return `\n\n${text.trim()}\n\n`;
  if (tag === 'span') return text;
  throw new Error(`Unsupported HTML tag: ${tag}`);
}
function html(text, file) {
  // Tutorial fields are HTML strings, not Svelte expressions.
  const ast = parse(text.replace(/\{/g, '&#123;').replace(/\}/g, '&#125;'), { modern: true });
  return clean(render(ast.fragment, {}, file));
}
function output(file, source, body) {
  outputs.set(file, `<!-- Generated from ${source}. Run: cd landing && pnpm docs:export -->\n\n${tidy(body).trim()}\n`);
}
function tidy(text) {
  // Collapse layout whitespace outside fenced code; examples stay byte-for-byte.
  let ticks = '';
  const lines = [];
  for (const line of text.split('\n')) {
    const marker = line.match(/^(`{3,})/);
    if (ticks) {
      lines.push(line);
      if (line === ticks) ticks = '';
    } else if (marker) {
      ticks = marker[1];
      lines.push(line);
    } else if (line.trim() || lines.at(-1) !== '') lines.push(line.trim());
  }
  return lines.join('\n');
}

for (const [i, topic] of topics.entries()) {
  const file = `docs/reference/${topic.id}.md`;
  const article = articles.find((a) => attr(a, 'id') === topic.id);
  // The colored group kicker is already represented by the index groups.
  const nodes = article.fragment.nodes.filter((n) => !(n.name === 'div' && attr(n, 'class').includes('uppercase')));
  const body = nodes.map((n) => render(n, docData, file)).join('');
  const nav = [i > 0 ? `[← ${topics[i - 1].title}](${topics[i - 1].id}.md)` : '',
    '[Documentation index](../README.md)',
    i + 1 < topics.length ? `[${topics[i + 1].title} →](${topics[i + 1].id}.md)` : ''].filter(Boolean).join(' · ');
  output(file, 'landing/src/Docs.svelte', `${nav}\n\n${tidy(body)}\n\n---\n\n${nav}`);
}

const tutorialFile = 'docs/TUTORIAL.md';
const tutorialSection = elements(landing.fragment, 'section').find((n) => attr(n, 'id') === 'docs');
const tutorialIntro = elements(tutorialSection, 'p').filter((n) => n.start < elements(tutorialSection, 'aside')[0].start);
let tutorial = `# ${homeData.docsText}\n\n[Documentation index](README.md) · [Overview](OVERVIEW.md)\n\n`;
tutorial += tutorialIntro.map((n) => render(n, homeData, tutorialFile)).join('');
tutorial += '\n\n## On this page\n\n';
for (const group of homeData.tutorial) {
  tutorial += `\n\n### ${group.group}\n\n` + group.chapters.map((ch) => `- [${ch.kicker}: ${ch.title}](#${ch.id})`).join('\n');
}
for (const group of homeData.tutorial) {
  tutorial += `\n\n## ${group.group}`;
  for (const ch of group.chapters) {
    tutorial += `\n\n<a id="${ch.id}"></a>\n\n### ${ch.kicker}: ${ch.title}\n\n${html(ch.lead, tutorialFile)}`;
    tutorial += fence(ch.code, ch.id === 'install' ? 'toml' : ch.id === 'deploy' ? 'bash' : 'rust');
    for (const kind of ['tip', 'note']) if (ch[kind])
      tutorial += `\n\n> [!${kind.toUpperCase()}]\n> ${html(ch[kind], tutorialFile)}\n`;
  }
}
const keepReading = elements(tutorialSection, 'div').find((n) => attr(n, 'class') === 'pt-2');
tutorial += '\n\n## Keep reading\n\n' + elements(keepReading, 'a')
  .map((n) => `- ${render(n, homeData, tutorialFile)}`).join('\n');
output(tutorialFile, 'landing/src/Landing.svelte', tidy(tutorial));

const overviewFile = 'docs/OVERVIEW.md';
const hero = elements(landing.fragment, 'main')[0];
let overview = `# ${homeData.finalTitle} — built from std up\n\n[Documentation index](README.md) · [Tutorial](TUTORIAL.md)\n\n`;
const badge = elements(hero, 'div').find((n) => attr(n, 'class').includes('uppercase'));
overview += render(badge, homeData, overviewFile);
overview += render(elements(hero, 'p')[0], homeData, overviewFile);
const stats = elements(hero, 'div').find((n) => attr(n, 'class').startsWith('flex gap-6'));
overview += '\n\n' + stats.fragment.nodes.filter((n) => n.type === 'RegularElement').map((n) => {
  const [amount, label] = n.fragment.nodes.filter((child) => child.type === 'RegularElement');
  return `- **${render(amount.fragment, homeData, overviewFile)}** ${render(label.fragment, homeData, overviewFile)}`;
}).join('\n');
overview += fence(homeData.installCmd, 'bash');
overview += fence(homeData.terminalLines.map((line) => line.text).join('\n'), 'text');
const sections = elements(landing.fragment, 'section');
overview += '\n\n## On this page\n\n' + sections.filter((n) => attr(n, 'id') && attr(n, 'id') !== 'docs')
  .map((n) => `- [${render(elements(n, 'h2')[0].fragment, homeData, overviewFile)}](#${attr(n, 'id')})`).join('\n');
for (const section of sections) {
  const id = attr(section, 'id');
  if (id === 'docs') {
    overview += '\n\n## Build your first app\n\nFollow the complete [Todo tutorial](TUTORIAL.md), from installation to deployment.\n';
  } else if (id === 'quickstart') {
    overview += '\n\n<a id="quickstart"></a>\n\n## Quick start\n\n';
    overview += render(elements(section, 'p')[0], homeData, overviewFile);
    const steps = findAll(section, (n) => n.type === 'EachBlock')[0];
    for (const step of value(steps.expression, homeData))
      overview += `\n\n### ${step.n}. ${step.title}\n\n${step.body}${fence(step.code, step.n === '1' ? 'toml' : step.n === '2' ? 'rust' : 'bash')}`;
  } else if (id === 'live') {
    overview += '\n\n<a id="live"></a>\n\n## Introspect a live app\n\n';
    overview += render(elements(section, 'p')[0], homeData, overviewFile);
    overview += '\n\nThe [website](https://enekos.github.io/sutegi/#live) provides an interactive URL field (default: `http://localhost:8080`). It shows the app name and version, route/model/tool counts, and the first eight routes with their descriptions. Browser access requires CORS. The panel currently sends no authorization header; for guarded apps, use the authenticated terminal request from [The agent surface](reference/agents.md).\n';
  } else {
    if (id) overview += `\n\n<a id="${id}"></a>\n`;
    overview += render(section, homeData, overviewFile);
  }
}
overview += render(elements(landing.fragment, 'footer')[0], homeData, overviewFile);
output(overviewFile, 'landing/src/Landing.svelte', tidy(overview));

let index = '# Sutegi documentation\n\nRead the full website content here on GitHub. Start with the [overview](OVERVIEW.md), follow the [Todo tutorial](TUTORIAL.md), or browse a reference topic below.\n\n[Project README](../README.md) · [Website](https://enekos.github.io/sutegi/) · [Examples](../examples) · [Agent contract](../AGENTS.md)\n';
for (const group of docData.nav)
  index += `\n\n## ${group.group}\n\n${group.items.map((t) => `- [${t.title}](reference/${t.id}.md)`).join('\n')}`;
index += '\n\n## In-depth guides\n\n';
for (const name of readdirSync(resolve(root, 'docs')).filter((n) => n.endsWith('.md') && !['README.md', 'OVERVIEW.md', 'TUTORIAL.md'].includes(n)).sort()) {
  const title = read(`docs/${name}`).match(/^# (.+)$/m)?.[1] ?? name;
  index += `- [${title}](${name})\n`;
}
index += '\n\n## Maintaining these docs\n\nThe overview, tutorial, reference pages, and this index are generated from the website source. Edit `landing/src/Landing.svelte` or `landing/src/Docs.svelte`, then run:\n\n```bash\ncd landing\npnpm install --frozen-lockfile\npnpm docs:export\npnpm docs:check\n```\n\nCommit the regenerated Markdown with the source change. The existing in-depth guides are maintained directly.\n';
output('docs/README.md', 'landing/src/{Landing,Docs}.svelte and docs/*.md', index);

// Check relative links in the exported navigation and prose. Code samples can
// contain Markdown as data, so ignore fenced blocks when scanning for links.
let links = 0;
for (const [file, content] of outputs) {
  const prose = content.replace(/^(`{3,})[^\n]*\n[\s\S]*?^\1\s*$/gm, '');
  for (const [, href] of prose.matchAll(/\]\(([^\s)]+)\)/g)) {
    if (/^[a-z]+:/i.test(href)) continue;
    const [path, anchor] = href.split('#');
    const target = path ? relative(root, resolve(root, dirname(file), path)) : file;
    if (!outputs.has(target) && !existsSync(resolve(root, target)))
      throw new Error(`Broken link in ${file}: ${href}`);
    if (anchor) {
      const destination = outputs.get(target) ?? read(target);
      // Website routes are exported with explicit anchors, independent of
      // GitHub's heading slug rules and future wording changes.
      if (!destination.includes(`<a id="${anchor}"></a>`))
        throw new Error(`Missing anchor in ${file}: ${href}`);
    }
    links++;
  }
}

let stale = false;
for (const [file, content] of outputs) {
  if (check) {
    let actual;
    try { actual = read(file); } catch { /* Report a missing export as stale. */ }
    if (actual !== content) { console.error(`Out of date: ${file}`); stale = true; }
  } else {
    mkdirSync(resolve(root, dirname(file)), { recursive: true });
    writeFileSync(resolve(root, file), content);
  }
}
if (stale) {
  console.error('Run pnpm docs:export and commit the generated Markdown.');
  process.exitCode = 1;
} else console.log(`${check ? 'Checked' : 'Exported'} ${topics.length} reference topics, ${homeData.tutorial.flatMap((g) => g.chapters).length} tutorial chapters, overview, and index (${links} relative links verified).`);

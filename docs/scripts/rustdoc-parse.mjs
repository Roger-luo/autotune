// Pure converter: rustdoc JSON (format_version 57) -> a Markdown body string
// for one crate. No file I/O — the Astro content loader in src/content.config.ts
// reads the JSON and feeds it through here, then renders the Markdown with
// Astro's own pipeline (Shiki, heading slugs, TOC). Regenerate the JSON with
// `pnpm api:rustdoc`.

// Sidebar order, matching the crate guides.
export const CRATE_ORDER = {
  autotune: 1, 'autotune-adaptor': 2, 'autotune-agent': 3, 'autotune-benchmark': 4,
  'autotune-config': 5, 'autotune-git': 6, 'autotune-implement': 7, 'autotune-init': 8,
  'autotune-judge': 9, 'autotune-mock': 10, 'autotune-plan': 11, 'autotune-score': 12,
  'autotune-state': 13, 'autotune-test': 14,
};

// Module-level item kinds we render, in display order.
const KIND_SECTIONS = [
  ['struct', 'Structs'],
  ['enum', 'Enums'],
  ['trait', 'Traits'],
  ['function', 'Functions'],
  ['macro', 'Macros'],
  ['type_alias', 'Type aliases'],
  ['constant', 'Constants'],
  ['static', 'Statics'],
  ['union', 'Unions'],
];

// Compiler-internal marker traits that add noise to the "Implements" list.
const INTERNAL_TRAITS = /^(StructuralPartialEq|StructuralEq)$/;

// ---------------------------------------------------------------------------
// Type / signature rendering
// ---------------------------------------------------------------------------

function argsToString(args) {
  if (!args) return '';
  if (args.angle_bracketed) {
    const parts = [];
    for (const a of args.angle_bracketed.args || []) {
      if (a.type) parts.push(typeToString(a.type));
      else if (a.lifetime) parts.push(a.lifetime);
      else if (a.const) parts.push(constToString(a.const));
    }
    for (const c of args.angle_bracketed.constraints || []) {
      parts.push(constraintToString(c));
    }
    return parts.length ? `<${parts.join(', ')}>` : '';
  }
  if (args.parenthesized) {
    const inputs = (args.parenthesized.inputs || []).map(typeToString).join(', ');
    const out = args.parenthesized.output ? ` -> ${typeToString(args.parenthesized.output)}` : '';
    return `(${inputs})${out}`;
  }
  return '';
}

function pathToString(p) {
  if (!p) return '_';
  const name = p.path.replace(/^crate::/, '');
  return `${name}${argsToString(p.args)}`;
}

function isMut(v) {
  return v.is_mutable ?? v.mutable ?? false;
}

function typeToString(t) {
  if (t == null) return '()';
  if (typeof t === 'string') return t;
  const k = Object.keys(t)[0];
  const v = t[k];
  switch (k) {
    case 'resolved_path': return pathToString(v);
    case 'generic': return v;
    case 'primitive': return v;
    case 'borrowed_ref': {
      const lt = v.lifetime ? `${v.lifetime} ` : '';
      const mut = isMut(v) ? 'mut ' : '';
      return `&${lt}${mut}${typeToString(v.type)}`;
    }
    case 'slice': return `[${typeToString(v)}]`;
    case 'array': return `[${typeToString(v.type)}; ${v.len}]`;
    case 'tuple': return `(${(v || []).map(typeToString).join(', ')})`;
    case 'raw_pointer': return `*${isMut(v) ? 'mut' : 'const'} ${typeToString(v.type)}`;
    case 'qualified_path': {
      const self = typeToString(v.self_type);
      const tr = v.trait ? pathToString(v.trait) : null;
      const base = tr ? `<${self} as ${tr}>` : self;
      return `${base}::${v.name}${argsToString(v.args)}`;
    }
    case 'impl_trait':
      return 'impl ' + (v || []).map(boundToString).filter(Boolean).join(' + ');
    case 'dyn_trait': {
      const traits = (v.traits || []).map((tt) => pathToString(tt.trait)).join(' + ');
      const lt = v.lifetime ? ` + ${v.lifetime}` : '';
      return `dyn ${traits}${lt}`;
    }
    case 'function_pointer': {
      const ins = (v.sig?.inputs || []).map(([, ty]) => typeToString(ty)).join(', ');
      const out = v.sig?.output ? ` -> ${typeToString(v.sig.output)}` : '';
      return `fn(${ins})${out}`;
    }
    case 'infer': return '_';
    default: return '_';
  }
}

function boundToString(b) {
  if (!b) return '';
  if (b.trait_bound) {
    const mod = b.trait_bound.modifier === 'maybe' ? '?' : '';
    return mod + pathToString(b.trait_bound.trait);
  }
  if (b.outlives) return b.outlives;
  return '';
}

function constraintToString(c) {
  const eq = c.binding?.equality;
  if (eq) {
    const val = eq.type ? typeToString(eq.type) : (eq.const ? constToString(eq.const) : '');
    return `${c.name} = ${val}`;
  }
  const con = c.binding?.constraint;
  if (con) return `${c.name}: ${con.map(boundToString).filter(Boolean).join(' + ')}`;
  return c.name;
}

function constToString(c) {
  if (typeof c === 'string') return c;
  return c.expr ?? c.value ?? '_';
}

function genericsToString(generics) {
  if (!generics) return '';
  const params = (generics.params || [])
    .filter((p) => !(p.kind?.type?.is_synthetic))
    .map((p) => {
      const k = Object.keys(p.kind)[0];
      if (k === 'lifetime') return p.name;
      if (k === 'const') return `const ${p.name}: ${typeToString(p.kind.const.type)}`;
      const bounds = (p.kind.type?.bounds || []).map(boundToString).filter(Boolean).join(' + ');
      return bounds ? `${p.name}: ${bounds}` : p.name;
    });
  return params.length ? `<${params.join(', ')}>` : '';
}

function selfString(ty) {
  if (ty && ty.borrowed_ref) {
    const r = ty.borrowed_ref;
    const lt = r.lifetime ? `${r.lifetime} ` : '';
    const mut = isMut(r) ? 'mut ' : '';
    return `&${lt}${mut}self`;
  }
  return 'self';
}

function fnSignature(name, fn) {
  const h = fn.header || {};
  const prefix = `${h.is_const ? 'const ' : ''}${h.is_async ? 'async ' : ''}${h.is_unsafe ? 'unsafe ' : ''}`;
  const g = genericsToString(fn.generics);
  const inputs = (fn.sig.inputs || []).map(([n, ty]) => (
    n === 'self' ? selfString(ty) : `${n}: ${typeToString(ty)}`
  )).join(', ');
  const out = fn.sig.output ? ` -> ${typeToString(fn.sig.output)}` : '';
  return `${prefix}fn ${name}${g}(${inputs})${out}`;
}

// ---------------------------------------------------------------------------
// Item rendering
// ---------------------------------------------------------------------------

const lookup = (index, id) => index[String(id)];

function docOf(item) {
  return item && item.docs ? item.docs.trim() + '\n' : '';
}

function publicMethods(index, implIds) {
  const methods = [];
  const traitImpls = new Set();
  for (const id of implIds || []) {
    const impl = lookup(index, id);
    if (!impl || !impl.inner.impl) continue;
    const inner = impl.inner.impl;
    if (inner.is_synthetic || inner.blanket_impl) continue;
    if (inner.trait) {
      const name = pathToString(inner.trait);
      if (!INTERNAL_TRAITS.test(name)) traitImpls.add(name);
      continue; // only list the trait name, not its methods
    }
    for (const itemId of inner.items || []) {
      const m = lookup(index, itemId);
      if (!m || m.visibility !== 'public') continue;
      if (!m.inner.function) continue;
      methods.push(m);
    }
  }
  return { methods, traitImpls: [...traitImpls].sort() };
}

function renderMethods(methods) {
  if (!methods.length) return '';
  let out = '\n#### Methods\n\n';
  for (const m of methods) {
    out += '```rust\n' + fnSignature(m.name, m.inner.function) + '\n```\n\n';
    if (m.docs) out += m.docs.trim() + '\n\n';
  }
  return out;
}

function renderTraitImpls(traitImpls) {
  if (!traitImpls.length) return '';
  return `\n**Implements:** ${traitImpls.map((t) => `\`${t}\``).join(', ')}\n`;
}

function renderStruct(index, item) {
  const s = item.inner.struct;
  const g = genericsToString(s.generics);
  const kind = Object.keys(s.kind)[0];
  let sig;
  let fieldsMd = '';
  if (kind === 'unit') {
    sig = `struct ${item.name}${g};`;
  } else if (kind === 'tuple') {
    const types = (s.kind.tuple || []).map((fid) => {
      const f = fid == null ? null : lookup(index, fid);
      return f ? typeToString(f.inner.struct_field) : '_';
    });
    sig = `struct ${item.name}${g}(${types.join(', ')});`;
  } else {
    sig = `struct ${item.name}${g} { /* fields */ }`;
    const fieldIds = s.kind.plain?.fields || [];
    const fields = [];
    for (const fid of fieldIds) {
      const f = lookup(index, fid);
      if (!f || f.visibility !== 'public') continue;
      const ty = typeToString(f.inner.struct_field);
      const d = f.docs ? ` — ${f.docs.trim().replace(/\n+/g, ' ')}` : '';
      fields.push(`- \`${f.name}: ${ty}\`${d}`);
    }
    if (s.kind.plain?.has_stripped_fields && !fields.length) {
      fieldsMd = '\n_All fields are private._\n';
    } else if (fields.length) {
      fieldsMd = '\n#### Fields\n\n' + fields.join('\n') + '\n';
    }
  }
  const { methods, traitImpls } = publicMethods(index, s.impls);
  return '```rust\n' + sig + '\n```\n\n' + docOf(item) + fieldsMd
    + renderMethods(methods) + renderTraitImpls(traitImpls);
}

function variantShape(index, v) {
  const kind = v.kind;
  if (kind === 'plain') return '';
  if (kind?.tuple) {
    const types = kind.tuple.map((fid) => {
      const f = fid == null ? null : lookup(index, fid);
      return f ? typeToString(f.inner.struct_field) : '_';
    });
    return `(${types.join(', ')})`;
  }
  if (kind?.struct) {
    const names = (kind.struct.fields || []).map((fid) => lookup(index, fid)?.name).filter(Boolean);
    return ` { ${names.join(', ')} }`;
  }
  return '';
}

function renderEnum(index, item) {
  const e = item.inner.enum;
  const g = genericsToString(e.generics);
  let out = '```rust\n' + `enum ${item.name}${g}` + '\n```\n\n' + docOf(item);
  const variants = [];
  for (const vid of e.variants || []) {
    const v = lookup(index, vid);
    if (!v || !v.inner.variant) continue;
    const shape = variantShape(index, v.inner.variant);
    const d = v.docs ? ` — ${v.docs.trim().replace(/\n+/g, ' ')}` : '';
    variants.push(`- \`${v.name}${shape}\`${d}`);
  }
  if (variants.length) out += '\n#### Variants\n\n' + variants.join('\n') + '\n';
  const { methods, traitImpls } = publicMethods(index, e.impls);
  return out + renderMethods(methods) + renderTraitImpls(traitImpls);
}

function renderTrait(index, item) {
  const t = item.inner.trait;
  const g = genericsToString(t.generics);
  const supers = (t.bounds || []).map(boundToString).filter(Boolean).join(' + ');
  const sig = `trait ${item.name}${g}${supers ? `: ${supers}` : ''}`;
  let out = '```rust\n' + sig + '\n```\n\n' + docOf(item);
  const methods = [];
  const assocTypes = [];
  for (const id of t.items || []) {
    const m = lookup(index, id);
    if (!m) continue;
    if (m.inner.function) methods.push(m);
    else if (m.inner.assoc_type) assocTypes.push(m);
  }
  if (assocTypes.length) {
    out += '\n#### Associated types\n\n';
    for (const a of assocTypes) {
      const at = a.inner.assoc_type;
      const bounds = (at.bounds || []).map(boundToString).filter(Boolean).join(' + ');
      out += `- \`type ${a.name}${bounds ? `: ${bounds}` : ''}\`` + (a.docs ? ` — ${a.docs.trim().replace(/\n+/g, ' ')}` : '') + '\n';
    }
  }
  if (methods.length) {
    out += '\n#### Required / provided methods\n\n';
    for (const m of methods) {
      out += '```rust\n' + fnSignature(m.name, m.inner.function) + '\n```\n\n';
      if (m.docs) out += m.docs.trim() + '\n\n';
    }
  }
  return out;
}

function renderFunction(item) {
  return '```rust\n' + fnSignature(item.name, item.inner.function) + '\n```\n\n' + docOf(item);
}

function renderTypeAlias(item) {
  const ta = item.inner.type_alias;
  const g = genericsToString(ta.generics);
  return '```rust\n' + `type ${item.name}${g} = ${typeToString(ta.type)};` + '\n```\n\n' + docOf(item);
}

function renderConstant(item) {
  const c = item.inner.constant;
  const val = c.const ? constToString(c.const) : '';
  return '```rust\n' + `const ${item.name}: ${typeToString(c.type)}${val ? ` = ${val}` : ''};` + '\n```\n\n' + docOf(item);
}

function renderStatic(item) {
  const s = item.inner.static;
  return '```rust\n' + `static ${isMut(s) ? 'mut ' : ''}${item.name}: ${typeToString(s.type)};` + '\n```\n\n' + docOf(item);
}

function renderMacro(item) {
  let body = typeof item.inner.macro === 'string' ? item.inner.macro : `${item.name}!`;
  if (body.length > 1600) body = body.slice(0, 1600) + '\n// …';
  return '```rust\n' + body + '\n```\n\n' + docOf(item);
}

function renderItem(index, item, kind) {
  switch (kind) {
    case 'struct': return renderStruct(index, item);
    case 'enum': return renderEnum(index, item);
    case 'trait': return renderTrait(index, item);
    case 'function': return renderFunction(item);
    case 'type_alias': return renderTypeAlias(item);
    case 'constant': return renderConstant(item);
    case 'static': return renderStatic(item);
    case 'macro': return renderMacro(item);
    default: return docOf(item);
  }
}

// ---------------------------------------------------------------------------
// Collection
// ---------------------------------------------------------------------------

const RELEVANT = new Set(KIND_SECTIONS.map(([k]) => k));

function collect(j) {
  const index = j.index;
  const seen = new Set();
  const collected = [];

  function pushItem(item, kind, path) {
    if (seen.has(item.id)) return;
    seen.add(item.id);
    collected.push({ item, kind, path });
  }

  function walk(modId, path) {
    const mod = lookup(index, modId);
    if (!mod || !mod.inner.module) return;
    for (const childId of mod.inner.module.items || []) {
      const it = lookup(index, childId);
      if (!it) continue;
      const kind = Object.keys(it.inner)[0];
      if (kind === 'module') {
        if (it.visibility === 'public') walk(childId, [...path, it.name]);
      } else if (kind === 'use') {
        const u = it.inner.use;
        if (u && u.id != null) {
          const tgt = lookup(index, u.id);
          if (tgt) {
            const tkind = Object.keys(tgt.inner)[0];
            if (tkind === 'module' && u.is_glob) walk(u.id, path);
            else if (RELEVANT.has(tkind) && tgt.visibility === 'public') pushItem(tgt, tkind, path);
          }
        }
      } else if (RELEVANT.has(kind) && it.visibility === 'public') {
        pushItem(it, kind, path);
      }
    }
  }

  walk(j.root, []);
  return { rootDocs: docOf(lookup(index, j.root)), collected };
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

// Convert one crate's rustdoc JSON object into a Markdown body (no frontmatter).
// Returns { markdown, itemCount }.
export function crateToMarkdown(j, crate) {
  const index = j.index;
  const { rootDocs, collected } = collect(j);
  const lines = [];
  lines.push('> Generated from rustdoc. Run `pnpm api:rustdoc` to refresh after changing the crate.');
  lines.push('');
  if (rootDocs) { lines.push(rootDocs); lines.push(''); }

  let itemCount = 0;
  for (const [kind, heading] of KIND_SECTIONS) {
    const items = collected
      .filter((c) => c.kind === kind)
      .sort((a, b) => a.item.name.localeCompare(b.item.name));
    if (!items.length) continue;
    lines.push(`## ${heading}`);
    lines.push('');
    for (const { item, path } of items) {
      itemCount++;
      lines.push(`### ${item.name}`);
      lines.push('');
      const fullPath = [crate.replace(/-/g, '_'), ...path, item.name].join('::');
      lines.push('`' + fullPath + '`');
      lines.push('');
      lines.push(renderItem(index, item, kind).trimEnd());
      lines.push('');
    }
  }

  if (!itemCount) {
    lines.push('_This crate exposes no public items in its rustdoc output._');
    lines.push('');
  }
  return { markdown: lines.join('\n') + '\n', itemCount };
}

/** Resolve relative Markdown links without letting browser URLs escape the workspace. */
export function resolveNotePath(href: string, sourcePath: string): string | null {
  if (/^[a-z][a-z\d+.-]*:|^\/\/|^#/i.test(href)) return null
  let decoded: string
  try {
    decoded = decodeURIComponent(href.split(/[?#]/)[0] ?? '')
  } catch {
    return null
  }
  const parts = decoded.startsWith('/') ? [] : sourcePath.split('/').slice(0, -1)
  for (const part of decoded.split('/')) {
    if (part === '..') {
      if (parts.length === 0) return null
      parts.pop()
    } else if (part !== '.' && part !== '') parts.push(part)
  }
  const path = parts.join('/')
  return /^(data|daily|archives)\/.+\.(md|mb)$/i.test(path) ? path : null
}

type Node = { type: string; value?: string; url?: string; children?: Node[] }
/** Work on Markdown text nodes, keeping code, existing links, and images intact. */
export function remarkNoteLinks() {
  return (tree: Node) => {
    const walk = (node: Node) => {
      if (!node.children || ['link', 'image', 'code', 'inlineCode'].includes(node.type)) return
      node.children = node.children.flatMap((child) => {
        if (child.type !== 'text' || !child.value) {
          walk(child)
          return [child]
        }
        const text = child.value
        const matches = [
          ...text.matchAll(/\[\[([^\[\]\n]+)\]\]|(?:^|(?<=\s))#([\p{L}\p{N}_][\p{L}\p{N}_/-]*)/gu),
        ]
        const nodes: Node[] = []
        let cursor = 0
        for (const match of matches) {
          const index = match.index
          if (index > cursor) nodes.push({ type: 'text', value: text.slice(cursor, index) })
          nodes.push({
            type: 'link',
            url: match[1]
              ? `/nole-wiki?target=${encodeURIComponent(match[1])}`
              : `/tag/${encodeURIComponent(match[2]!)}`,
            children: [{ type: 'text', value: match[0] }],
          })
          cursor = index + match[0].length
        }
        if (cursor < text.length) nodes.push({ type: 'text', value: text.slice(cursor) })
        return nodes
      })
    }
    walk(tree)
  }
}

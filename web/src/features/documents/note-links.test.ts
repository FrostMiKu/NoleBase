import { describe, expect, it } from 'vitest'
import { remarkNoteLinks, resolveNotePath } from './note-links'
describe('note links', () => {
  it('resolves relative Markdown paths only within managed note directories', () => {
    expect(resolveNotePath('../data/中文%20笔记.md', 'daily/2026-10-22.md')).toBe(
      'data/中文 笔记.md',
    )
    expect(resolveNotePath('other.mb', 'data/note.md')).toBe('data/other.mb')
    for (const href of [
      '../../secret.md',
      '../config/AGENTS.md',
      'javascript:alert(1)',
      'https://example.com',
      '//host/a.md',
    ])
      expect(resolveNotePath(href, 'data/a.md')).toBeNull()
  })
  it('links references in text while preserving code and existing links', () => {
    const tree = {
      type: 'root',
      children: [
        { type: 'paragraph', children: [{ type: 'text', value: 'See [[Note]] #rust' }] },
        { type: 'inlineCode', value: '[[Note]]' },
        { type: 'link', url: 'https://example.com', children: [{ type: 'text', value: '#rust' }] },
      ],
    }
    remarkNoteLinks()(tree)
    expect(tree.children[0]?.children).toEqual([
      { type: 'text', value: 'See ' },
      {
        type: 'link',
        url: '/nole-wiki?target=Note',
        children: [{ type: 'text', value: '[[Note]]' }],
      },
      { type: 'text', value: ' ' },
      { type: 'link', url: '/tag/rust', children: [{ type: 'text', value: '#rust' }] },
    ])
    expect(tree.children[1]?.value).toBe('[[Note]]')
    expect(tree.children[2]?.children).toEqual([{ type: 'text', value: '#rust' }])
  })
})

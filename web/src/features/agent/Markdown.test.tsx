import { cleanup, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it } from 'vitest'

import { Markdown } from './Markdown'

afterEach(cleanup)

describe('Markdown', () => {
  it('renders headings, lists, code, and GFM tables', () => {
    render(
      <Markdown
        text={[
          '## 结论',
          '',
          '- 第一条 **重点**',
          '- 第二条',
          '',
          '```rust',
          'fn main() {}',
          '```',
          '',
          '| 列 A | 列 B |',
          '| --- | --- |',
          '| 1 | 2 |',
          '',
          '~~划掉~~ 和 [链接](https://example.com)',
        ].join('\n')}
      />,
    )
    expect(screen.getByRole('heading', { name: '结论' })).toBeTruthy()
    expect(screen.getAllByRole('listitem').length).toBe(2)
    expect(screen.getByRole('table')).toBeTruthy()
    expect(screen.getByText('fn main() {}')).toBeTruthy()
    expect(screen.getByText('链接')).toBeTruthy()
  })

  it('keeps non-markdown syntax (BBCode/wikilinks) as plain text', () => {
    const { container } = render(<Markdown text={'[box]frame[/box] links to [[note-a]]'} />)
    expect(container.textContent).toBe('[box]frame[/box] links to [[note-a]]')
  })
})

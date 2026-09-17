# QA Checklists

Interactive, clickable-checkbox test checklists for DoD Studio. One
`.json` file per checklist in this folder — drop a new one in and it just
shows up, no rebuild step.

## Running it

```
node docs/qa_checklists/server.js
```

Then open the URL it prints (`http://localhost:5680/` by default, or pass a
port: `node docs/qa_checklists/server.js 8080`). The server has zero
dependencies — just Node's built-ins — and does two things: serves
`manifest.html`, and serves `GET /api/checklists`, which lists and parses
every `*.json` file in this folder on each request. That's what makes new
checklists appear automatically — refresh the page after adding a file, no
Claude involvement needed.

Progress (which boxes are checked) is saved per-checklist in the browser's
localStorage, keyed by the checklist's `id` — so it's local to whichever
browser/machine you're running the server from.

## Adding a checklist

Drop a new file here matching the schema below, then just refresh the page.

```json
{
  "id": "unique-stable-slug",
  "label": "Short tab label",
  "title": "Full title shown in the checklist header",
  "subtitle": "One or two sentences of context",
  "sections": [
    {
      "title": "Section Name",
      "note": "optional small flag, e.g. \"needs a real capture batch\"",
      "items": ["One test case per string", "..."]
    }
  ]
}
```

- `id` is the localStorage key — keep it stable once a checklist has real
  progress checked off, or that progress becomes unreachable (a renamed `id`
  is effectively a new, empty checklist).
- `note` on a section is optional — used sparingly, for a section that needs
  something beyond just clicking around the app (e.g. a real capture batch).

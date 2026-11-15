#!/usr/bin/env python3
"""Keep the FAQ's structured data in step with the visible FAQ in index.html.

Search engines only accept FAQPage data that matches what the page shows, so after editing a
question or answer in the FAQ section, run this from the branch root:

    python3 tools/faq-jsonld.py

It gives every <details> in #faq-list an id (for links like #faq-is-local-playlist-free) if it has
none, and rewrites the <script type="application/ld+json" id="ld-faq"> block. Standard library only.
"""

import html
import json
import pathlib
import re
import sys

PAGE = pathlib.Path(__file__).resolve().parent.parent / "index.html"


def text(fragment: str) -> str:
    """Plain text of an HTML fragment, whitespace collapsed."""
    return re.sub(r"\s+", " ", html.unescape(re.sub(r"<[^>]+>", "", fragment))).strip()


def slug(question: str) -> str:
    s = re.sub(r"[^a-z0-9]+", "-", question.lower()).strip("-")
    if len(s) > 48:
        s = s[:48].rsplit("-", 1)[0]  # whole words only
    return "faq-" + s


def main() -> int:
    page = PAGE.read_text(encoding="utf-8")
    m = re.search(r'(<div class="faq" id="faq-list">)(.*?)(\n      </div>\n    </div>\n  </section>)', page, re.S)
    if not m:
        print("FAQ list not found in index.html", file=sys.stderr)
        return 1
    faq = m.group(2)
    entries = []

    def visit(d: re.Match) -> str:
        attrs, body = d.group(1), d.group(2)
        q = re.search(r"<summary>(.*?)</summary>", body, re.S)
        a = re.search(r'<div class="answer">(.*?)</div>', body, re.S)
        if not q or not a:
            raise SystemExit("A <details> in the FAQ lacks a <summary> or an answer <div>")
        question, answer = text(q.group(1)), text(a.group(1))
        entries.append({
            "@type": "Question",
            "name": question,
            "acceptedAnswer": {"@type": "Answer", "text": answer},
        })
        if "id=" not in attrs:
            attrs = f' id="{slug(question)}"' + attrs
        return f"<details{attrs}>{body}</details>"

    faq = re.sub(r"<details([^>]*)>(.*?)</details>", visit, faq, flags=re.S)
    data = {"@context": "https://schema.org", "@type": "FAQPage", "mainEntity": entries}
    block = json.dumps(data, ensure_ascii=False, indent=2).replace("</", "<\\/")
    page = page[: m.start(2)] + faq + page[m.end(2):]
    page, n = re.subn(
        r'(<script type="application/ld\+json" id="ld-faq">\n).*?(\n</script>)',
        lambda s: s.group(1) + block + s.group(2),
        page,
        flags=re.S,
    )
    if n != 1:
        print('The <script type="application/ld+json" id="ld-faq"> block was not found', file=sys.stderr)
        return 1
    PAGE.write_text(page, encoding="utf-8")
    print(f"{len(entries)} questions written to the FAQPage data")
    return 0


if __name__ == "__main__":
    sys.exit(main())

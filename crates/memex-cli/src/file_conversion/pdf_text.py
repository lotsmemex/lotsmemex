"""Honor tagged PDF replacement text without guessing or OCR correction."""


def actual_text_by_page(pdf):
    from pdfminer.pdftypes import resolve1
    from pdfminer.utils import decode_text

    pages = {}
    active = set()

    def walk(value, page_id=None, replacement=None):
        obj = resolve1(value)
        if isinstance(obj, list):
            for child in obj:
                walk(child, page_id, replacement)
        elif isinstance(obj, int):
            if replacement is not None:
                replacement[1].add((page_id, obj))
        elif isinstance(obj, dict):
            identity = id(obj)
            if identity in active:
                raise ValueError("ambiguous_pdf_actual_text")
            active.add(identity)
            if "Pg" in obj:
                page_id = getattr(obj["Pg"], "objid", None)
            own = "ActualText" in obj and replacement is None
            if own:
                replacement = (decode_text(resolve1(obj["ActualText"])), set())
            if "MCID" in obj:
                walk(resolve1(obj["MCID"]), page_id, replacement)
            else:
                walk(obj.get("K", []), page_id, replacement)
            if own:
                text, references = replacement
                scope = {p for p, _ in references}
                if len(scope) != 1 or None in scope:
                    raise ValueError("unsupported_pdf_actual_text_scope")
                target = pages.setdefault(next(iter(scope)), {})
                key = len(target)
                for _, mcid in references:
                    if mcid in target:
                        raise ValueError("ambiguous_pdf_actual_text")
                    target[mcid] = (key, text)
            active.remove(identity)

    # Inspect complete scope before page filtering: a cross-page ActualText
    # cannot be emitted once on each page without duplicating the replacement.
    walk(pdf.doc.catalog.get("StructTreeRoot", {}))
    return pages


def pdf_page_lines(page):
    """Correct text and its original geometry together; never reconstruct CMaps."""
    from pdfminer.pdfdevice import PDFDevice
    from pdfminer.pdfinterp import PDFPageInterpreter

    if not hasattr(page.pdf, "_memex_lines"):
        page.pdf._memex_lines = {}
    if page.page_number in page.pdf._memex_lines:
        return page.pdf._memex_lines[page.page_number]
    if not hasattr(page.pdf, "_memex_actual_text"):
        page.pdf._memex_actual_text = actual_text_by_page(page.pdf)
    replacements = page.pdf._memex_actual_text.get(page.page_obj.pageid, {})

    class ScopeGuard(PDFDevice):
        def __init__(self, manager):
            super().__init__(manager)
            self.scopes = []

        def begin_tag(self, tag, props=None):
            if any(m in replacements for m in self.scopes):
                # pdfplumber's current MCID tracker is not a stack. Reject
                # unsupported nesting instead of leaking covered glyph text.
                raise ValueError("unsupported_pdf_actual_text_nesting")
            self.scopes.append(props.get("MCID") if isinstance(props, dict) else None)

        def end_tag(self):
            if self.scopes:
                self.scopes.pop()

    if replacements:
        PDFPageInterpreter(page.pdf.rsrcmgr, ScopeGuard(page.pdf.rsrcmgr)).process_page(page.page_obj)
    result = []
    lines = [{"text": [], "chars": []}]
    emitted = set()
    seen = set()
    # Keep pdfplumber's geometry-based whitespace, but replace each marked
    # content sequence once. The PDF's ActualText can contain more/fewer
    # Unicode characters than its rendered glyphs (ligatures, faulty CMaps).
    for text, char in page.get_textmap().tuples:
        if char is None and text == "\n":
            result.append(text)
            lines.append({"text": [], "chars": []})
            continue
        marked = replacements.get(char.get("mcid")) if char is not None else None
        if marked is None:
            value = text
        else:
            key, actual = marked
            seen.add(char["mcid"])
            value = ""
            if key not in emitted:
                value = actual
                emitted.add(key)
        result.append(value)
        lines[-1]["text"].append(value)
        if char is not None:
            lines[-1]["chars"].append(char)
    # Layout may normalize a whitespace-only glyph into generated whitespace.
    if any(replacements[i][1].strip() for i in set(replacements) - seen):
        raise ValueError("unresolved_pdf_actual_text")
    import statistics
    records = []
    for line in lines:
        chars = line["chars"]
        text = "".join(line["text"]).strip()
        if not chars or not text:
            continue
        ordered = sorted(chars, key=lambda c: c["x0"])
        size = statistics.median(c["size"] for c in chars)
        # A large internal gutter may be columns or a table. Never infer a
        # reading order from a flattened line spanning it.
        gutter = any(b["x0"] - a["x1"] > size * 2
                     for a, b in zip(ordered, ordered[1:]))
        records.append(dict(text=text, x0=min(c["x0"] for c in chars),
                            x1=max(c["x1"] for c in chars), top=min(c["top"] for c in chars),
                            bottom=max(c["bottom"] for c in chars),
                            size=size, gutter=gutter))
    value = "".join(result), len(emitted), records
    page.pdf._memex_lines[page.page_number] = value
    return value


def read_pdf_page(page):
    raw, count, _ = pdf_page_lines(page)
    return raw, count


def pagination_number(page):
    import re
    _, _, lines = pdf_page_lines(page)
    if lines:
        last = lines[-1]
        if (re.fullmatch(r"\d{1,4}", last["text"]) and last["top"] > page.height * .88
                and abs((last["x0"] + last["x1"]) / 2 - page.width / 2) < page.width * .08):
            return int(last["text"])
    return None


def verified_pagination(page):
    # Geometry alone cannot distinguish an isolated publication year. Require
    # a three-page consecutive run in the full PDF, even inside a short chapter.
    index = page.page_number - 1
    candidates = {}
    for i in range(max(0, index - 2), min(len(page.pdf.pages), index + 3)):
        value = pagination_number(page.pdf.pages[i])
        if value is not None:
            candidates[i] = value - i
    return any(all(i in candidates and candidates[i] == candidates.get(index)
                   for i in range(start, start + 3))
               for start in range(max(0, index - 2), index + 1))


def verified_edge_furniture(page, line, side):
    """Exact repeated, isolated small-type edge text, not arbitrary numbers.

    Require three nearby pages with matching position/type and body separation.
    Repeated body lines, refrains and notes are not evidence. Text footers cannot
    be distinguished safely from repeated explanatory notes by geometry alone;
    keep them. Consecutive pagination has its separate verified path.
    """
    import re
    import statistics

    if side != "head":
        return False

    def candidate(p):
        _, _, records = pdf_page_lines(p)
        if len(records) < 4:
            return None
        edge = records[0]
        body = records[1:]
        size = statistics.median(r["size"] for r in body)
        gap = body[0]["top"] - edge["bottom"]
        if (edge["size"] >= size * .9 or gap < size * 2
                or abs((edge["x0"] + edge["x1"]) / 2 - p.width / 2) > p.width * .12
                or len(edge["text"]) > 100 or not re.search(r"[^\W\d_]", edge["text"])
                or re.search(r"[.!?。！？]$", edge["text"])
                or re.match(r"^(?:\d|[(*•†‡#|\[（]|[-+]\s)", edge["text"])
                or edge["bottom"] > p.height * .1):
            return None
        return edge

    own = candidate(page)
    if own is None or own != line:
        return False
    matches = 0
    index = page.page_number - 1
    for other in page.pdf.pages[max(0, index - 2):index + 3]:
        edge = candidate(other)
        if (edge and edge["text"] == line["text"]
                and abs(other.width - page.width) < 1 and abs(other.height - page.height) < 1
                and all(abs(edge[k] - line[k]) < 2 for k in ("x0", "x1", "top", "size"))):
            matches += 1
    return matches >= 3


def pdf_paragraphs(page):
    """Reading projection from line geometry, independent of language or title.

    Keep source text immutable. Do not invent headings from font sizes: isolate
    them, and let the existing explicit outline supply navigation semantics.
    """
    import statistics
    raw, count, lines = pdf_page_lines(page)
    lines = list(lines)
    removed = []
    if pagination_number(page) is not None and verified_pagination(page):
        removed.append(lines.pop())
    for side in ("head", "foot"):
        if lines and verified_edge_furniture(page, lines[0 if side == "head" else -1], side):
            removed.append(lines.pop(0 if side == "head" else -1))
    if not lines:
        return dict(raw=raw, blocks=[], first=None, last=None, replacements=count,
                    removed=len(removed), breaks=[])
    size = statistics.median(l["size"] for l in lines)
    body = [l for l in lines if abs(l["size"] - size) < size * .15]
    if not body:
        return dict(raw=raw, blocks=[l["text"] for l in lines], first=None, last=None,
                    replacements=count, removed=len(removed), breaks=["mixed_font_no_body"])
    left = min(l["x0"] for l in body)
    right = max(l["x1"] for l in body)
    # Sparse title-page credits are independent lines, not wrapped prose.
    # Require both a large title and a narrow text cluster; a short final prose
    # page with ordinary body typography must still retain its paragraphs.
    if (len(lines) <= 6 and max(l["size"] for l in lines) >= size * 1.5
            and right - left < page.width * .5):
        return dict(raw=raw, blocks=[l["text"] for l in lines], first=None, last=None,
                    replacements=count, removed=len(removed), breaks=["sparse_title_page"])
    spans = [b["top"] - a["top"] for a, b in zip(body, body[1:])
             if size < b["top"] - a["top"] < size * 3]
    pitch = statistics.median(spans) if spans else size * 1.5
    blocks, breaks = [], []
    previous = None
    for line in lines:
        reason = None
        if previous is not None:
            if abs(line["size"] - previous["size"]) > size * .2:
                reason = "font_change"
            elif line["top"] - previous["top"] > pitch * 1.35:
                reason = "vertical_gap"
            elif size * .8 < line["x0"] - left < size * 4:
                reason = "first_line_indent"
            elif (right - left > page.width * .65 and right - previous["x1"] > size * 3
                  and abs(line["x0"] - left) < size * .5):
                reason = "short_previous_line"
        if previous is None or reason:
            blocks.append(line["text"])
            if reason:
                breaks.append(reason)
        else:
            blocks[-1] += "\n" + line["text"]
        previous = line
    first, last = dict(lines[0]), dict(lines[-1])
    for line in (first, last):
        line.update(left=left, right=right, body_size=size, page_width=page.width,
                    page_height=page.height, line_count=len(body),
                    full_lines=sum(abs(l["x0"] - left) < size * .5 and right - l["x1"] < size * 1.5
                                   for l in body),
                    ambiguous=any(l.get("gutter", False) for l in lines)
                              or any(l["x0"] - left > size * 4 for l in body),
                    page_number=page.page_number)
    return dict(raw=raw, blocks=blocks, first=first, last=last,
                replacements=count, removed=len(removed), breaks=breaks)


def join_pdf_pages(pages):
    """Join a continued prose paragraph only with matching page-edge geometry.

    Call separately for each explicit chapter, so a page join never crosses it.
    Uncertain joins remain separate and are exposed to the quality audit.
    """
    blocks = []
    previous = None
    joins = 0
    for page in pages:
        current = list(page["blocks"])
        a = previous["last"] if previous else None
        b = page["first"]
        if a and b and blocks and current:
            import re
            size = a["body_size"]
            # Linguistic hints veto unsafe joins; they never establish a
            # boundary by themselves. Keep punctuation and hyphens unchanged.
            terminal = re.search(r'[.!?。！？;；:：][\"\u201d\u2019\')\]]*$', a["text"])
            structural = any(re.match(r'^(?:[#|*•†‡]|[-+]\s|\d+[.)、]\s*)', line["text"])
                             for line in (a, b))
            if (not terminal and not structural
                    and not a.get("ambiguous") and not b.get("ambiguous")
                    and a.get("line_count", 0) >= 3 and b.get("line_count", 0) >= 3
                    and (a["right"] - a["left"] > a["page_width"] * .65
                         or (a.get("full_lines", 0) >= 2 and b.get("full_lines", 0) >= 2))
                    and b.get("page_number") == a.get("page_number", -2) + 1
                    and abs(a["page_width"] - b["page_width"]) < 1
                    and abs(a["page_height"] - b["page_height"]) < 1
                    and abs(a["size"] - size) < size * .15
                    and abs(b["size"] - size) < size * .15
                    and abs(b["body_size"] - size) < size * .15
                    and a["bottom"] > a["page_height"] * .82
                    and b["top"] < b["page_height"] * .18
                    and a["right"] - a["left"] > a["page_width"] * .45
                    and b["right"] - b["left"] > b["page_width"] * .45
                    and abs(a["left"] - b["left"]) < size * .5
                    and abs(a["right"] - b["right"]) < size
                    and a["right"] - a["x1"] < size * 1.5
                    and abs(b["x0"] - b["left"]) < size * .5):
                blocks[-1] += "\n" + current.pop(0)
                joins += 1
        blocks.extend(current)
        previous = page
    return "\n\n".join(blocks), joins

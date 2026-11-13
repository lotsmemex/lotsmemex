// Synthetic demo library. No real MemeX data: every title, source and body here is invented
// so the mobile UI can be reviewed on a phone before the private Mac gateway is connected.

const SOURCES = [
  { name: 'https://arxiv.org', type: 'preprint' },
  { name: 'https://36kr.com', type: 'webpage' },
  { name: 'https://www.36kr.com', type: 'webpage' },
  { name: 'https://m.36kr.com', type: 'webpage' },
  { name: 'https://sspai.com', type: 'blog_post' },
  { name: 'https://mp.weixin.qq.com', type: 'webpage' },
  { name: 'https://andrewkelley.me', type: 'blog_post' },
  { name: 'urn:zotero:group:42', type: 'journal_article' },
  { name: 'urn:zotero:group:42', type: 'thesis' },
];
// Long-tail sources with a single item each (folded into 其他来源 in the sidebar), and a second
// kind of local file next to the urn:sha256 transcript (both read 本地文件).
const ONE_OFF = { 7: 'https://one-off-blog.example', 19: 'https://news.sina.com.cn', 20: 'https://finance.sina.com.cn',
  33: 'https://weekly.example', 48: 'file:///Users/demo/notes/reading-list.pdf', 61: 'https://www.economist.com' };
// Synthetic group tree (印象笔记 / Zotero / 我的抓取): item i joins the folders whose rule matches.
export const GROUPS = [
  { id: 'g_ev', name: '印象笔记', kind: 'user', parent_id: null },
  { id: 'g_ev_ai', name: 'AI 研究', kind: 'collection', parent_id: 'g_ev' },
  { id: 'g_ev_llm', name: '大模型', kind: 'collection', parent_id: 'g_ev_ai' },
  { id: 'g_ev_agent', name: '智能体', kind: 'collection', parent_id: 'g_ev_ai' },
  { id: 'g_ev_city', name: '城市观察', kind: 'collection', parent_id: 'g_ev' },
  { id: 'g_ev_read', name: '读书笔记', kind: 'collection', parent_id: 'g_ev' },
  { id: 'g_zot', name: 'Zotero 我的文库', kind: 'zotero_group', parent_id: null },
  { id: 'g_zot_rag', name: '检索增强', kind: 'collection', parent_id: 'g_zot' },
  { id: 'g_zot_thesis', name: '学位论文', kind: 'collection', parent_id: 'g_zot' },
  { id: 'g_cap', name: '我的抓取', kind: 'user', parent_id: null },
];
const GROUP_RULES = [['g_ev_llm', (i) => i % 5 === 0], ['g_ev_agent', (i) => i % 9 === 4], ['g_ev_city', (i) => i % 5 === 1], ['g_ev_read', (i) => i % 7 === 2],
  ['g_zot_rag', (i) => i % 6 === 3], ['g_zot_thesis', (i) => i % 11 === 4], ['g_cap', (i) => i < 40 && i % 2 === 1]];

const TOPICS = [
  '大规模语言模型在长文档检索增强生成中的上下文压缩方法综述与实证评估',
  '城市更新中的公共空间设计：以三座城市为例的比较研究',
  'Attention Is Not All You Need: Revisiting Transformer Efficiency',
  '生成式人工智能在教育中的应用与挑战',
  '从社交媒体数据看城市情绪的时空演化',
  'A Survey on Retrieval-Augmented Generation',
  '气候变化下的城市韧性评价框架',
  '社区商业空间的再生策略与实践',
  '大语言模型时代的人机协作范式',
  'My Thoughts on the Bun Rust Rewrite',
  '古典文献数字化中的版本校勘问题',
  'Local-first Software: You Own Your Data, in Spite of the Cloud',
  '慢阅读：在信息洪流中重建注意力',
  'Zotero 式资料管理的十年演变',
  '图书馆元数据标准的演进与互操作',
  'Designing Calm Technology for Personal Knowledge',
];

const EN_TITLES_ZH = {
  'Attention Is Not All You Need: Revisiting Transformer Efficiency': '注意力并非一切：重新审视 Transformer 的效率',
  'A Survey on Retrieval-Augmented Generation': '检索增强生成综述',
  'My Thoughts on the Bun Rust Rewrite': '我对 Bun 用 Rust 重写的看法',
  'Local-first Software: You Own Your Data, in Spite of the Cloud': '本地优先软件：数据归你所有，即便有云',
  'Designing Calm Technology for Personal Knowledge': '为个人知识设计安静的技术',
};

const TAGS = ['检索增强', '综述', '城市更新', '公共空间', '教育', '人机协作', '本地优先', '阅读', '元数据', '系统编程'];

const BODY_ZH = [
  '随着大规模语言模型在知识密集型任务中的广泛应用，如何在长文档场景下有效地进行检索增强生成（RAG）成为研究热点。由于模型的上下文窗口存在限制，直接将检索到的长文本输入模型往往会引入大量冗余信息，导致生成质量下降和计算成本上升。',
  '本文系统梳理了近年来提出的上下文压缩方法，按照压缩策略将其分为基于抽取、基于生成和基于结构化建模三类，并在多个公开数据集上进行了实证评估。实验结果表明，不同方法在不同任务场景下各有优势。',
  '基于抽取的方法保留原文片段，可追溯性最好；基于生成的方法压缩率最高，但存在改写失真的风险；基于结构化建模的方法在多跳问答上表现最稳定。',
  '我们进一步讨论了压缩比例与答案忠实度之间的权衡，并给出面向个人资料库的实践建议：优先保证引用可回到原文的具体位置。',
  '（以上为合成演示正文，不来自你的资料库。）',
  '① 这里的“上下文窗口”指模型一次能读入的最大文本长度。（合成注释）',
  '② 文中提到的公开数据集，名称列在原文附录里。（合成注释）',
];
const BODY_EN = [
  'As large language models are widely applied to knowledge-intensive tasks, effective retrieval-augmented generation (RAG) over long documents has become an active research area. Because context windows are limited, feeding long retrieved passages directly into a model often introduces redundant information.',
  'This article surveys recently proposed context-compression methods, grouping them into extractive, abstractive and structure-based approaches, and evaluates them empirically on several public datasets.',
  'Extractive methods keep original spans and are the easiest to trace; abstractive methods compress the most but risk distortion; structure-based methods are the most stable on multi-hop questions.',
  'We further discuss the trade-off between compression ratio and answer faithfulness, and recommend that citations in a personal library always lead back to an exact location in the source.',
  '(Synthetic demo text — not from your library.)',
  '① “Context window” here means the most text a model can read in one go. (Synthetic note.)',
  '② The public datasets mentioned are named in the appendix of the source. (Synthetic note.)',
];

function rng(seed) {
  let s = seed >>> 0;
  return () => ((s = (s * 1664525 + 1013904223) >>> 0) / 2 ** 32);
}

function pad(n) { return String(n).padStart(2, '0'); }

export function buildLibrary(count = 1222) {
  const r = rng(20261101);
  const items = [];
  const start = Date.UTC(2026, 9, 31);
  for (let i = 0; i < count; i += 1) {
    const pick = SOURCES[Math.floor(r() * SOURCES.length)];
    const src = ONE_OFF[i] ? { name: ONE_OFF[i], type: ONE_OFF[i].startsWith('file:') ? 'document' : 'webpage' } : pick;
    const base = TOPICS[i % TOPICS.length];
    const title = i < TOPICS.length ? base : `${base}（${Math.floor(i / TOPICS.length) + 1}）`;
    const d = new Date(start - Math.floor(i * 0.9 + r() * 3) * 86400000);
    const tags = TAGS.filter(() => r() < 0.18).slice(0, 3);
    const added = i % 97 === 5 ? null : `${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())} ${pad(9 + (i % 10))}:${pad((i * 7) % 60)}`;
    const pub = new Date(d.getTime() - Math.floor(r() * 400) * 86400000);
    // Imported (Zotero) items all joined the library in one recent batch; own captures joined when captured.
    const imported = src.name.startsWith('urn:zotero:');
    items.push({
      id: `demo-${i + 1}`,
      title,
      source: src.name,
      type: src.type,
      // An import gets its capture time when it is converted, so captured_at alone cannot keep own captures on top.
      added: imported ? `2026-11-02 23:${pad(10 + (i % 40))}` : added,
      origin: imported ? 'import' : 'capture',
      addedAt: imported ? `2026-11-02 23:${pad(10 + (i % 40))}` : added,
      published: i % 13 === 7 ? null : `${pub.getUTCFullYear()}-${pad(pub.getUTCMonth() + 1)}-${pad(pub.getUTCDate())}`,
      groups: GROUP_RULES.filter(([, f]) => f(i)).map(([g]) => g),
      unread: r() < 0.12,
      // demo-1 (long, many key terms) always has its translation so 对照 can be reviewed.
      translation: i === 0 || r() < 0.7 ? 'done' : r() < 0.5 ? 'running' : 'none',
      tags, english: /^[A-Za-z]/.test(title),
      // Candidate title-only translation for English titles (list + reader lead with it).
      translatedTitle: EN_TITLES_ZH[base] ? `${EN_TITLES_ZH[base]}${i < TOPICS.length ? '' : `（${Math.floor(i / TOPICS.length) + 1}）`}` : null,
      revision: 1,
    });
  }
  // A locally imported Chinese transcript (~100 KB, many blank-line paragraphs) whose source is a
  // content hash, like a PDF imported on the Mac. Type is what the backend sends; no translation.
  items.splice(1, 0, {
    id: 'demo-local-zh', title: '（合成）城市研究圆桌讨论逐字稿', source: `urn:sha256:${'3f9a1c07'.repeat(8)}`, type: 'webpage',
    added: '2026-10-31 08:40', addedAt: '2026-10-31 08:40', published: null, duration: 3599, groups: ['g_ev_city'], unread: true, translation: 'none', tags: ['逐字稿'], revision: 1, longZh: true,
  });
  // A paired bilingual book (reader_v1): English original edition + Chinese translation edition.
  // The canonical Markdown (what item.get / search see) is the Chinese translation, as on the Mac.
  items.splice(5, 0, {
    id: 'demo-book', title: 'The Lighthouse Keeper’s Year (synthetic)', source: `urn:sha256:${'5be1a0c4'.repeat(8)}`, type: 'book',
    added: '2026-10-30 21:05', addedAt: '2026-10-30 21:05', published: '1931-01-01', groups: ['g_ev_read'], unread: true, translation: 'none', tags: ['阅读'], revision: 1,
    book: true, translatedTitle: '守灯人的一年（合成）',
  });
  // 长文阅读 sample: a synthetic podcast transcript (timestamped headings, 「名字：」 speaker turns).
  items.splice(4, 0, {
    id: 'demo-talk', title: '（合成）播客逐字稿：长文档问答里的上下文压缩', source: 'https://podcast.example.invalid/ep42', type: 'podcast',
    added: '2026-11-01 21:30', addedAt: '2026-11-01 21:30', published: '2026-10-30', duration: 3626, groups: ['g_ev_read'], unread: true, translation: 'none', tags: ['阅读'], revision: 1, talk: true,
  });
  // 古籍 sample (简体 Markdown, chapters 「第 NN 章」, **bold** and <font color=red>) for 简繁 / 竖排 / 寒蝉拙楷.
  // The scripture lines are the public-domain 道德经; every 注 is invented for this demo.
  items.splice(3, 0, {
    id: 'demo-guji', title: '（合成）道德经 · 古籍阅读样例', source: `urn:sha256:${'6d0a3b19'.repeat(8)}`, type: 'book',
    added: '2026-10-29 20:10', addedAt: '2026-10-29 20:10', published: null, groups: ['g_ev_read'], unread: true, translation: 'none', tags: ['阅读'], revision: 1, guji: true,
  });
  // A Markdown-rich synthetic article (headings, nested lists, quote, wide table, code, links, image).
  items.splice(2, 0, {
    id: 'demo-md', title: '（合成）Markdown 渲染样例：本地优先笔记的整理方法', source: 'https://sspai.com', type: 'blog_post',
    added: '2026-10-31 09:15', addedAt: '2026-10-31 09:15', published: '2026-10-20', groups: ['g_cap'], unread: true, translation: 'none', tags: ['阅读'], revision: 1, markdown: true,
  });
  // 说话人名字 sample: an English podcast transcript whose (synthetic) translation renders the speakers'
  // names differently (Thorsten → 托尔斯滕, Quinn → 奎因). Kept last so earlier positions do not move.
  items.push({
    id: 'demo-talk-en', title: '(Synthetic) Podcast: keeping the lamp lit', source: 'https://podcast.example.invalid/ep7', type: 'podcast',
    added: '2026-09-01 20:00', addedAt: '2026-09-01 20:00', published: '2026-08-30', duration: null, groups: ['g_ev_read'], unread: true, translation: 'done', tags: ['阅读'], revision: 1,
    talkEn: true, english: true, translatedTitle: '（合成）播客：让灯一直亮着',
  });
  return items;
}

// Synthetic paired book (invented text, written for this demo). Chapters are [key, title, level,
// blocks, counterpart]; a chapter's Markdown is its blocks joined by blank lines. The translation
// has front matter and the original an appendix without counterparts; chapter 2 is long enough to
// need several chunks at the demo chunk size.
const STORM_EN = Array.from({ length: 18 }, (_, i) => `Log, night ${i + 1}. The wind came from the north-east again and the keeper climbed the ninety-two steps twice before midnight to trim the wick. He wrote that the sea sounded like furniture being moved in a distant house, and that the gulls had gone inland. (Synthetic text, paragraph ${i + 1}.)`);
const STORM_ZH = Array.from({ length: 18 }, (_, i) => `日志，第 ${i + 1} 夜。风又从东北方向来，午夜前守灯人两次爬上九十二级台阶去修剪灯芯。他写道，海的声音像远处一所房子里有人在搬动家具，海鸥都已飞往内陆。（合成文本，第 ${i + 1} 段。）`);
export const DEMO_BOOK = {
  original: { language: 'en', chapters: [
    ['ch-1', 'Chapter One: Arrival', 1, ['# Chapter One: Arrival',
      'The boat left him on the rock in late March, with two crates of oil, a sack of flour and a borrowed almanac. The tower was older than the harbour it guarded.',
      'He counted the steps on the first evening: ninety-two.',
      'They were worn hollow in the middle by keepers he would never meet.',
      '(Synthetic demo text — not from any real book.)'], 'zh-1'],
    ['ch-2', 'Chapter Two: Winter Storms', 1, ['# Chapter Two: Winter Storms', 'By November the light was the only warm thing on the island.', ...STORM_EN], 'zh-2'],
    ['ch-2-log', 'The Storm Log', 2, ['## The Storm Log', 'On the worst night the glass of the lantern room sang. He tied himself to the rail and kept the lamp burning until the grey morning came up behind the waves.',
      '① The rail is the iron hand-rail of the gallery outside the lantern room. (Synthetic note.)',
      '② “Grey morning”: the log gives no clock time for the dawn. (Synthetic note.)'], 'zh-2-log'],
    // A paragraph broken across a page edge (to be joined by the chapter repair).
    ['ch-3', 'Chapter Three: The Return', 1, ['# Chapter Three: The Return',
      'In spring the relief boat came back. He handed over the almanac, now full of his',
      'own small handwriting, and did not look back at the tower until the harbour wall hid it.'], 'zh-3'],
    ['appendix', 'A Note on Lamps', 1, ['# A Note on Lamps', 'Paraffin lamps of this kind burned about a pint an hour. This appendix exists only in the original edition.'], null],
  ] },
  translation: { language: 'zh', chapters: [
    ['zh-front', '译者前言', 1, ['# 译者前言', '这部小书讲的是一个守灯人在孤岛上度过的一年。译文力求平实，章节与原书一一对应。', '（合成演示文本，不来自任何真实书籍。）'], null],
    ['zh-1', '第一章 抵达', 1, ['# 第一章 抵达',
      '三月下旬，船把他留在礁石上，还有两箱灯油、一袋面粉和一本借来的历书。',
      '这座灯塔比它守护的港口还要古老。',
      '第一个傍晚他数了台阶：九十二级，中间被他永远不会见到的前任守灯人磨得凹了下去。',
      '译注：原书只写“ninety-two”，未说明是否包括灯室的梯子。（合成译注）',
      '（合成演示文本，不来自任何真实书籍。）'], 'ch-1'],
    ['zh-2', '第二章 冬日风暴', 1, ['# 第二章 冬日风暴', '到了十一月，灯光成了岛上唯一温暖的东西。', ...STORM_ZH], 'ch-2'],
    ['zh-2-log', '风暴日志', 2, ['## 风暴日志', '最糟的那一夜，灯室的玻璃嗡嗡作响。他把自己绑在栏杆上，让灯一直亮着，直到灰色的早晨从浪头后面升起。',
      '① 栏杆指灯室外环廊的铁扶手。（合成注释）',
      '② “灰色的早晨”：日志没有写下天亮的钟点。（合成注释）'], 'ch-2-log'],
    ['zh-3', '第三章 归来', 1, ['# 第三章 归来', '春天，换班的船回来了。他交还了那本历书，里面已写满他细小的字迹；直到港口的防波堤挡住了灯塔，他才回头看了一眼。'], 'ch-3'],
  ] },
};
// What the synthetic chapter repair changes in the text (a new item revision): the broken paragraph
// of chapter 3 is joined. Other chapters keep their bytes.
export const DEMO_BOOK_REPAIRED = { original: { 'ch-3': ['# Chapter Three: The Return',
  'In spring the relief boat came back. He handed over the almanac, now full of his own small handwriting, and did not look back at the tower until the harbour wall hid it.'] } };
// Synthetic paragraph correspondence by ORIGINAL chapter key. Rows are [original blocks [i, j] |
// null, translation blocks [i, j] | null, kind, relation] (inclusive block indexes; the source turns
// them into chapter-relative UTF-8 byte ranges). `repaired` is the index after a chapter repair.
const m = (o, t, kind = 'paragraph') => [o, t, kind, 'matched'];
const stormRows = (gaps) => [m([0, 0], [0, 0], 'heading'), m([1, 1], [1, 1]),
  ...STORM_EN.flatMap((_, i) => (gaps.includes(i) ? [[[i + 2, i + 2], null, 'paragraph', 'unresolved'], [null, [i + 2, i + 2], 'paragraph', 'unresolved']] : [m([i + 2, i + 2], [i + 2, i + 2])]))];
export const DEMO_ALIGN = {
  // 1:1 heading, 1:n, n:1, a translator's note only in the translation, 1:1.
  'ch-1': { rows: [m([0, 0], [0, 0], 'heading'), m([1, 1], [1, 2]), m([2, 3], [3, 3]), [null, [4, 4], 'note', 'translation_only'], m([4, 4], [5, 5])] },
  // Partial: nights 7 and 13 are unresolved on both sides (separate rows, never a pair).
  'ch-2': { rows: stormRows([6, 12]), repaired: stormRows([]) },
  'ch-2-log': { rows: [m([0, 0], [0, 0], 'heading'), m([1, 1], [1, 1]), m([2, 2], [2, 2], 'note'), m([3, 3], [3, 3], 'note')] },
  // No index yet; the repair joins the broken paragraph (changed revision) and matches it.
  'ch-3': { rows: null, repaired: [m([0, 0], [0, 0], 'heading'), m([1, 1], [1, 1])] },
  'appendix': { rows: null },
};

// ~100 KB of synthetic Chinese transcript paragraphs (UTF-8), separated by blank lines.
export function longChineseTranscript() {
  const lines = [
    '主持人：欢迎各位参加今天的圆桌讨论，我们先请每位嘉宾简单介绍自己关注的城市更新问题。',
    '嘉宾甲：我主要关注老旧街区的公共空间，尤其是步行尺度和日常使用之间的关系，这些问题往往比宏大叙事更具体。',
    '嘉宾乙：我更关心数据，比如人流、停留时间和噪声，这些指标能帮助我们判断一个改造是不是真的改善了生活。',
    '主持人：那么在具体项目里，数据和居民感受出现分歧的时候，你们通常怎么处理？',
    '嘉宾甲：先把分歧记录下来，再回到现场多看几次，很多时候是指标选错了，而不是居民的感受错了。',
  ];
  const paras = [];
  for (let i = 0; paras.join('\n\n').length < 36500; i += 1) {
    paras.push(i % 12 === 0 ? `## 第 ${i / 12 + 1} 部分` : `${lines[i % lines.length]}（合成逐字稿，第 ${i + 1} 段。）`);
  }
  return paras.join('\n\n');
}

export function bodyOf(_item, lang) {
  return lang === 'en' ? BODY_EN : BODY_ZH;
}

export function summaryOf(item) {
  return `本文围绕「${item.title.replace(/（\d+）$/, '')}」展开，梳理主要观点并给出可追溯到原文的结论。（合成摘要，只读）`;
}

// Long synthetic AI minutes (labelled as such) so the expandable minutes panel can be reviewed.
export function minutesOf(item) {
  const topic = item.title.replace(/（\d+）$/, '');
  return [
    `（合成纪要，仅供演示）围绕「${topic}」，全文可以概括为**五个要点**：`,
    '### 要点',
    '1. **问题背景**：长文档检索增强生成受限于模型的*上下文窗口*，冗余文本会降低回答质量，也会增加计算成本。\n2. **方法分类**：作者把现有压缩方法分成三类：\n   - 抽取式：保留原文片段\n   - 生成式：由模型重写\n   - 结构化建模：利用文档结构\n3. **实证结果**：抽取式最容易追溯到原文；生成式压缩率最高但可能改写失真；结构化建模在多跳问答上最稳定。\n4. **权衡讨论**：压缩比例越高，答案忠实度越难保证。\n5. **实践建议**：先用抽取式保留可追溯片段，再按需要叠加结构化索引；生成式压缩只用于浏览，`不用于引用`。',
    '> 待核实：文中的数据集规模与评测指标没有给出来源链接，阅读时可以在原文第 3 节核对。',
  ].join('\n\n');
}

// Synthetic AI insights (labelled 合成示例 in the UI). Every quote is copied verbatim from the
// synthetic bodies above and block_index counts their blank-line blocks, like the agent's output.
export function insightsOf(item) {
  if (!item.english) {
    return {
      highlights: { state: 'succeeded', reason: null, items: [
        { block_index: 0, quote: '如何在长文档场景下有效地进行检索增强生成（RAG）成为研究热点', style: 'highlight', label: '研究问题' },
        { block_index: 2, quote: '基于抽取的方法保留原文片段，可追溯性最好', style: 'positive', label: '优点' },
        { block_index: 2, quote: '基于生成的方法压缩率最高，但存在改写失真的风险', style: 'negative', label: '风险' },
        { block_index: 3, quote: '优先保证引用可回到原文的具体位置', style: 'bold', label: '结论' },
      ] },
      quotes: { state: 'succeeded', reason: null, items: [
        { block_index: 3, quote: '我们进一步讨论了压缩比例与答案忠实度之间的权衡，并给出面向个人资料库的实践建议：优先保证引用可回到原文的具体位置。', translation_zh: null, why: '把可追溯性放在压缩率之上，是全文的落点。' },
        { block_index: 2, quote: '基于抽取的方法保留原文片段，可追溯性最好；基于生成的方法压缩率最高，但存在改写失真的风险；基于结构化建模的方法在多跳问答上表现最稳定。', translation_zh: null, why: '一句话概括三类方法各自的取舍。' },
      ] },
      fact_check: { state: 'succeeded', reason: null, basis: 'model_knowledge', items: [] },
    };
  }
  return {
    highlights: { state: 'succeeded', reason: null, items: [
      { block_index: 0, quote: 'effective retrieval-augmented generation (RAG) over long documents has become an active research area', style: 'highlight', label: '研究问题' },
      { block_index: 2, quote: 'Extractive methods keep original spans and are the easiest to trace', style: 'positive', label: '优点' },
      { block_index: 2, quote: 'abstractive methods compress the most but risk distortion', style: 'negative', label: '风险' },
      { block_index: 3, quote: 'citations in a personal library always lead back to an exact location in the source', style: 'bold', label: '结论' },
    ] },
    quotes: { state: 'succeeded', reason: null, items: [
      { block_index: 3, quote: 'We further discuss the trade-off between compression ratio and answer faithfulness, and recommend that citations in a personal library always lead back to an exact location in the source.',
        translation_zh: '我们进一步讨论了压缩比例与答案忠实度之间的权衡，并建议个人资料库中的引用始终能回到原文的确切位置。', why: '把可追溯性放在压缩率之上，是全文的落点。' },
      { block_index: 2, quote: 'Extractive methods keep original spans and are the easiest to trace; abstractive methods compress the most but risk distortion; structure-based methods are the most stable on multi-hop questions.',
        translation_zh: '抽取式方法保留原文片段，最容易追溯；生成式方法压缩率最高，但有失真的风险；结构化方法在多跳问答上最稳定。', why: '一句话概括三类方法各自的取舍。' },
      { block_index: 0, quote: 'Because context windows are limited, feeding long retrieved passages directly into a model often introduces redundant information.',
        translation_zh: '由于上下文窗口有限，把检索到的长段落直接输入模型，往往会引入冗余信息。', why: '点明问题的起因。' },
    ] },
    fact_check: { state: 'succeeded', reason: null, basis: 'model_knowledge', items: [
      { block_index: 0, quote: 'Because context windows are limited', verdict: 'supported', confidence: 'high',
        explanation: '主流语言模型的上下文窗口确实有固定上限，这是公认的技术事实。（合成核查）' },
      { block_index: 2, quote: 'structure-based methods are the most stable on multi-hop questions', verdict: 'disputed', confidence: 'medium',
        explanation: '不同评测对多跳问答的结论并不一致，“最稳定”取决于数据集和指标，文中没有给出依据。（合成核查）' },
      { block_index: 1, quote: 'evaluates them empirically on several public datasets', verdict: 'unverifiable', confidence: 'low',
        explanation: '文中没有列出数据集名称与规模，仅凭模型知识无法判断。（合成核查）' },
      { block_index: 3, quote: 'citations in a personal library always lead back to an exact location in the source', verdict: 'opinion', confidence: 'high',
        explanation: '这是作者的建议，属于价值判断，不是可核查的事实。（合成核查）' },
    ] },
  };
}

export const SAMPLE_CHAT = [
  { who: 'user', text: '三种压缩方法有什么区别？', time: '今天 14:32' },
  {
    who: 'memex',
    text: '这篇文章把上下文压缩方法分为**三类**：\n1. **抽取式**：从原文选择关键片段，最容易追溯；\n2. **生成式**：由模型重写组织内容，压缩率最高；\n3. **结构化建模**：利用文档结构，多跳问答最稳定。',
    refs: ['§2.1', '§3.4'],
    time: '今天 14:32',
  },
];

// Synthetic Markdown article for the renderer review (demo-md).
// 长文阅读 (synthetic, written for this demo): four timestamped sections, speaker turns 「名字：」 at a
// paragraph start, continued paragraphs without a label. Uses the demo's 重点 terms so AI highlights exist.
const TALK_LINES = [
  '我们一开始就讨论检索增强生成，因为没有上限的时候，大家根本不会去想成本。',
  '冗余信息会把上下文塞满，最后的账单反而更高，回答也更散。',
  '上下文压缩不是删字，而是先挑出能追溯到原文的句子。',
  '我们做了一轮实证评估，抽取式的结果最容易核对。',
  '可追溯性比压缩率重要，读者得能点回原文。',
  '生成式压缩看起来短，但改写失真的风险一直都在。',
  '多跳问答是最难的场景，前后两段要同时留住。',
  '忠实度和篇幅总要权衡，我们宁可多留一点原句。',
  '最后每个结论都要带引用，不然没人敢用。',
];
const TALK_SECS = [['00:00', '开场：为什么要压缩上下文'], ['12:40', '便宜的方法为什么最后更贵'], ['31:05', '抽取式与生成式'], ['48:20', '给读者留下原文']];
export const TALK_SAMPLE = (() => {
  const out = []; const who = ['Quinn', '林岚']; let n = 0;
  TALK_SECS.forEach(([t, title], si) => {
    out.push(`## ${t} ${title}`);
    let sp = si % 2;
    for (let k = 0; k < 7; k += 1) {
      if (k === 2 || k === 4 || k === 6) sp = 1 - sp;
      const first = k === 0 || k === 2 || k === 4 || k === 6;
      const line = `${TALK_LINES[(n * 4 + si) % TALK_LINES.length]}${TALK_LINES[(n * 4 + si + 4) % TALK_LINES.length]}（合成逐字稿 ${n + 1}）`;
      out.push(first ? `${who[sp]}：${line}` : line);
      n += 1;
    }
  });
  return out.join('\n\n');
})();

// English podcast + its synthetic translation, block for block (说话人名字). Invented text.
const TALK_EN_TURNS = [
  ['Thorsten', 'Welcome back. Today we talk about keeping a lighthouse lamp lit through a winter storm.', '欢迎回来。今天我们聊聊怎样在冬季风暴里让灯塔的灯一直亮着。'],
  ['Quinn', 'Thanks for having me. The first thing every keeper learns is to trim the wick before midnight.', '谢谢邀请。每个守灯人学到的第一件事，就是午夜前修剪灯芯。'],
  [null, 'Otherwise the flame turns black and the glass fogs over within an hour.', '否则火焰会发黑，一小时内玻璃就会蒙上一层雾。'],
  ['Thorsten', 'How often did you climb the ninety-two steps on a bad night?', '在糟糕的夜里，你要爬几次那九十二级台阶？'],
  ['Quinn', 'Twice, sometimes three times. You learn to count them in the dark.', '两次，有时三次。你会学会在黑暗里数台阶。'],
  ['Thorsten', 'And the log? Did you write every hour?', '那日志呢？你每小时都写吗？'],
  ['Quinn', 'Every hour, even when the only entry was “wind north-east, lamp burning”. (Synthetic transcript.)', '每小时都写，哪怕只有一句“东北风，灯亮着”。（合成逐字稿）'],
];
// Three parts (long enough to scroll the 「正在说」 bubble into view).
const TALK_EN_PARTS = [['00:00', 'Keeping the lamp lit', '让灯一直亮着'], ['14:10', 'The log', '日志'], ['27:45', 'Leaving the rock', '离开礁石']];
export const TALK_EN = TALK_EN_PARTS.flatMap(([t, h]) => [`## ${t} ${h}`, ...TALK_EN_TURNS.map(([n, en]) => (n ? `${n}: ${en}` : en))]).join('\n\n');
export const TALK_EN_ZH = TALK_EN_PARTS.flatMap(([t, , h]) => [`## ${t} ${h}`, ...TALK_EN_TURNS.map(([n, , zh]) => (n ? `${{ Thorsten: '托尔斯滕', Quinn: '奎因' }[n]}：${zh}` : zh))]).join('\n\n');

export const GUJI_SAMPLE = [
  '# 第 01 章',
  '**道可道，非常道；名可名，非常名。**',
  '无名天地之始，有名万物之母。故常无欲，以观其妙；常有欲，以观其徼。此两者同出而异名，同谓之玄。<font color=red>玄之又玄，众妙之门。</font>',
  '注：此章言道体本无名相，后学当于动静之间体认，勿执于文字。万物皆从此出，故曰众妙之门。（合成注文，仅供演示）',
  '# 第 02 章',
  '天下皆知美之为美，斯恶已；皆知善之为善，斯不善已。**故有无相生，难易相成，长短相较，高下相倾，音声相和，前后相随。**',
  '是以圣人处无为之事，行不言之教；万物作焉而不辞，生而不有，为而不恃，<font color="red">功成而弗居。</font>夫唯弗居，是以不去。',
  '注：美恶、善否、有无、难易，皆相对而发；圣人处之以无为，故能长久。乾坤之理，亦复如是。（合成注文，仅供演示）',
  '# 第 03 章',
  '不尚贤，使民不争；不贵难得之货，使民不为盗；不见可欲，使民心不乱。',
  '是以圣人之治，虚其心，实其腹，弱其志，强其骨。常使民无知无欲，使夫智者不敢为也。为无为，则无不治。',
].join('\n\n');

export const MARKDOWN_SAMPLE = [
  '## 为什么要本地优先',
  '本地优先的笔记有三个好处：**数据归你所有**、*离线也能用*、同步出错时可以回到 `原始文件`。参见 [Local-first software](https://www.inkandswitch.com/local-first/)。',
  '1. 先收集，不急着分类\n2. 每周整理一次\n   - 删掉重复的条目\n   - 给值得精读的加上标签\n     - 标签不超过三个\n3. 每月回顾',
  '> 好的笔记系统，应该让你**更少**地想起它。\n> ——合成引文',
  '| 方法 | 适合 | 代价 | 备注说明（这一列故意很长，用来把表格撑得比屏幕还宽，看它是否只在自己的框里横向滚动） |\n| --- | --- | :---: | --- |\n| 抽取式 | 引用 | 低 | 保留原文片段，最容易追溯到原始位置 |\n| 生成式 | 浏览 | 中 | 压缩率最高，但可能改写失真 |',
  '---',
  '代码里的星号保持原样：`**不是粗体**`，转义的 \\*也不是斜体\\*，HTML 也只当文字：<b onclick="x()">不加粗</b>。',
  '```js\nconst notes = load(\'*.md\'); // 星号 * 原样显示\n```',
  '![示意图：笔记的流转](https://example.com/flow.png)',
  '不安全的链接会被停用：[点我](javascript:alert(1))；安全的链接正常打开：<https://example.com/notes>。',
  '（以上为合成演示正文，不来自你的资料库。）',
].join('\n\n');

// 重点标注 v2 (key terms): one row per term, original-side English keyword ↔ Chinese term on the
// Chinese side. Spans are exact UTF-8 byte offsets into split_blocks text, like the agent's.
const TERMS = [
  ['retrieval-augmented', '检索增强生成', 'highlight', '研究问题'], ['redundant', '冗余信息', 'negative', '问题'],
  ['context-compression', '上下文压缩', 'bold', '主题'], ['empirically', '实证评估', 'highlight', '方法'],
  ['Extractive', '可追溯性', 'positive', '优点'], ['distortion', '改写失真', 'negative', '风险'],
  ['multi-hop', '多跳问答', 'highlight', '场景'], ['faithfulness', '忠实度', 'bold', '权衡'], ['citations', '引用', 'positive', '结论'],
];
const utf8 = (s) => new TextEncoder().encode(s).length;
const spanOf = (block, term, i) => { const at = block.indexOf(term); return at < 0 ? null : { text: term, block: i, start: utf8(block.slice(0, at)), end: utf8(block.slice(0, at)) + utf8(term) }; };
export function termHighlights(origBlocks, transBlocks, english) {
  const items = [];
  origBlocks.forEach((b, i) => {
    for (const [en, zh, style, label] of TERMS) {
      const original = spanOf(b, english ? en : zh, i);
      const translation = transBlocks && transBlocks[i] != null ? spanOf(transBlocks[i], english ? zh : en, i) : null;
      if (!original && !translation) continue;
      items.push({ block_index: i, quote: original ? original.text : translation.text, style, label, original, translation });
    }
  });
  return items;
}

//! 来源 in the sidebar: the agent's source facets (one per origin, e.g.
//! `https://m.36kr.com`) grouped by registrable domain under a short site name
//! (36氪), sub-sites folded into their group, and sources with a single item
//! folded into 「其他来源」. Presentation only: filters still name one origin.

use crate::model::FacetEntry;
use memex_client::OTHER_SOURCE;

/// Two-label public suffixes seen in the library (registrable domain = one more label).
const TWO_LABEL_SUFFIXES: &[&str] = &[
    "com.cn", "net.cn", "org.cn", "gov.cn", "edu.cn", "ac.cn", "com.hk", "org.hk", "com.tw", "org.tw", "co.uk", "ac.uk", "org.uk", "co.jp", "ne.jp", "or.jp",
    "com.au", "co.kr", "com.sg", "co.nz", "com.br",
];

/// Short site names: registrable domain (or a host that is its own publication) → name.
/// Shared with the phone; anything not listed shows its registrable domain.
pub const SITE_NAMES: &[(&str, &str)] = &[
    ("mp.weixin.qq.com", "微信公众号"),
    ("36kr.com", "36氪"),
    ("sspai.com", "少数派"),
    ("huxiu.com", "虎嗅"),
    ("zhihu.com", "知乎"),
    ("thepaper.cn", "澎湃新闻"),
    ("caixin.com", "财新"),
    ("jiqizhixin.com", "机器之心"),
    ("qbitai.com", "量子位"),
    ("geekpark.net", "极客公园"),
    ("ifanr.com", "爱范儿"),
    ("latepost.com", "晚点 LatePost"),
    ("infoq.cn", "InfoQ"),
    ("ithome.com", "IT之家"),
    ("jiemian.com", "界面新闻"),
    ("yicai.com", "第一财经"),
    ("cls.cn", "财联社"),
    ("guancha.cn", "观察者网"),
    ("douban.com", "豆瓣"),
    ("weibo.com", "微博"),
    ("bilibili.com", "哔哩哔哩"),
    ("xiaohongshu.com", "小红书"),
    ("163.com", "网易"),
    ("sina.com.cn", "新浪"),
    ("qq.com", "腾讯"),
    ("sohu.com", "搜狐"),
    ("youtube.com", "YouTube"),
    ("youtu.be", "YouTube"),
    ("x.com", "X"),
    ("twitter.com", "X"),
    ("github.com", "GitHub"),
    ("arxiv.org", "arXiv"),
    ("medium.com", "Medium"),
    ("substack.com", "Substack"),
    ("nytimes.com", "纽约时报"),
    ("ft.com", "金融时报"),
    ("wsj.com", "华尔街日报"),
    ("bloomberg.com", "彭博"),
    ("reuters.com", "路透"),
    ("economist.com", "经济学人"),
    ("wikipedia.org", "维基百科"),
];

/// `www.m.36kr.com` → `36kr.com`; `news.sina.com.cn` → `sina.com.cn`. A host
/// listed in `SITE_NAMES` as its own publication (mp.weixin.qq.com) stays whole.
/// `None` for labels that are not host names (urn:…, 其他).
pub fn registrable_domain(host: &str) -> Option<String> {
    let host = host.split('/').next()?.split(':').next()?.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() || !host.contains('.') || host.parse::<std::net::IpAddr>().is_ok() || !host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-') {
        return None;
    }
    if let Some((own, _)) = SITE_NAMES.iter().find(|(d, _)| d.matches('.').count() >= 2 && (host == *d || host.ends_with(&format!(".{d}")))) {
        return Some(own.to_string());
    }
    let labels: Vec<&str> = host.split('.').collect();
    let take = if labels.len() >= 3 && TWO_LABEL_SUFFIXES.contains(&labels[labels.len() - 2..].join(".").as_str()) { 3 } else { 2 };
    Some(labels[labels.len().saturating_sub(take)..].join("."))
}

/// The short name shown for a registrable domain.
pub fn site_name(domain: &str) -> String {
    SITE_NAMES.iter().find(|(d, _)| *d == domain).map(|(_, n)| n.to_string()).unwrap_or_else(|| domain.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceGroup {
    /// Registrable domain, or the facet label for non-host sources.
    pub key: String,
    pub name: String,
    pub count: usize,
    /// The origins in this group, largest first.
    pub members: Vec<FacetEntry>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceTree {
    /// Groups with more than one item, largest first.
    pub groups: Vec<SourceGroup>,
    /// 「其他来源」: single-item sources and items without a source.
    pub others: Vec<FacetEntry>,
}

impl SourceTree {
    pub fn others_count(&self) -> usize {
        self.others.iter().map(|f| f.count).sum()
    }
}

pub fn group_sources(sources: &[FacetEntry]) -> SourceTree {
    let mut groups: Vec<SourceGroup> = Vec::new();
    let mut others = Vec::new();
    for f in sources {
        if f.label == OTHER_SOURCE {
            others.push(f.clone());
            continue;
        }
        let (key, name) = match registrable_domain(&f.label) {
            Some(d) => (d.clone(), site_name(&d)),
            None => (f.label.clone(), f.label.clone()),
        };
        match groups.iter_mut().find(|g| g.key == key) {
            Some(g) => {
                g.count += f.count;
                g.members.push(f.clone());
            }
            None => groups.push(SourceGroup { key, name, count: f.count, members: vec![f.clone()] }),
        }
    }
    let (mut keep, single): (Vec<_>, Vec<_>) = groups.into_iter().partition(|g| g.count > 1);
    others.extend(single.into_iter().flat_map(|g| g.members));
    for g in &mut keep {
        g.members.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.label.cmp(&b.label)));
    }
    keep.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
    others.sort_by(|a, b| (a.label == OTHER_SOURCE).cmp(&(b.label == OTHER_SOURCE)).then_with(|| a.label.cmp(&b.label)));
    SourceTree { groups: keep, others }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(label: &str, count: usize) -> FacetEntry {
        FacetEntry { key: format!("https://{label}"), label: label.into(), count }
    }

    #[test]
    fn registrable_domains_fold_sub_sites_and_keep_own_publications() {
        assert_eq!(registrable_domain("www.36kr.com").as_deref(), Some("36kr.com"));
        assert_eq!(registrable_domain("m.36kr.com:443").as_deref(), Some("36kr.com"));
        assert_eq!(registrable_domain("news.sina.com.cn").as_deref(), Some("sina.com.cn"));
        assert_eq!(registrable_domain("www.bbc.co.uk").as_deref(), Some("bbc.co.uk"));
        assert_eq!(registrable_domain("mp.weixin.qq.com").as_deref(), Some("mp.weixin.qq.com"), "WeChat articles are not 腾讯");
        assert_eq!(registrable_domain("new.qq.com").as_deref(), Some("qq.com"));
        assert_eq!(registrable_domain("urn:zotero:group:42"), None);
        assert_eq!(registrable_domain("127.0.0.1"), None);
        assert_eq!(registrable_domain(OTHER_SOURCE), None);
        assert_eq!(site_name("36kr.com"), "36氪");
        assert_eq!(site_name("weekly.example"), "weekly.example");
    }

    #[test]
    fn groups_sum_their_members_and_singletons_fold_into_others() {
        let tree = group_sources(&[
            f("36kr.com", 3),
            f("www.36kr.com", 5),
            f("m.36kr.com", 1),
            f("sspai.com", 2),
            f("one.example", 1),
            f("mp.weixin.qq.com", 1),
            FacetEntry { key: String::new(), label: OTHER_SOURCE.into(), count: 4 },
            f("urn:zotero:group:7", 9),
        ]);
        let names: Vec<(&str, usize)> = tree.groups.iter().map(|g| (g.name.as_str(), g.count)).collect();
        assert_eq!(names, [("36氪", 9), ("urn:zotero:group:7", 9), ("少数派", 2)]);
        assert_eq!(tree.groups[0].members.iter().map(|m| m.label.as_str()).collect::<Vec<_>>(), ["www.36kr.com", "36kr.com", "m.36kr.com"]);
        assert_eq!(tree.others.iter().map(|m| m.label.as_str()).collect::<Vec<_>>(), ["mp.weixin.qq.com", "one.example", OTHER_SOURCE]);
        assert_eq!(tree.others_count(), 6);
        // Every item is counted exactly once.
        assert_eq!(tree.groups.iter().map(|g| g.count).sum::<usize>() + tree.others_count(), 26);
    }
}

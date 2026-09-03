use std::collections::BTreeSet;

use crate::{
    domain::WritingContext,
    tools::{KnowledgeBundle, SearchHit},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaterialSearchPlan {
    pub query_terms: Vec<String>,
    pub corpus_query: String,
    pub web_query: String,
    pub display_query: String,
    pub year_from: Option<i32>,
    pub year_to: Option<i32>,
    pub strict_year_label: Option<String>,
    pub max_results: usize,
    pub topic_context: Option<String>,
}

#[derive(Clone, Debug)]
pub struct MaterialSearchService {
    max_results: usize,
}

impl MaterialSearchService {
    pub fn new(max_results: usize) -> Self {
        Self { max_results }
    }

    pub fn build_plan(&self, message: &str, context: &WritingContext) -> MaterialSearchPlan {
        let (year_from, year_to) = extract_year_range(message);
        let topic_context = [
            context.topic.as_deref(),
            context.initial_idea.as_deref(),
            context.selected_path.as_deref(),
            context.research_question.as_deref(),
            context
                .extra
                .get("theory_entry")
                .and_then(serde_json::Value::as_str),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
        let basis = [message, topic_context.as_str()]
            .into_iter()
            .filter(|part| !part.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let query_terms = query_terms(&basis, context);
        let strict_year_label = match (year_from, year_to) {
            (Some(start), Some(end)) => Some(format!("{start}-{end}")),
            (Some(start), None) => Some(format!("{start} 年以来")),
            (None, Some(end)) => Some(format!("{end} 年以前")),
            _ => None,
        };
        let mut corpus_terms = query_terms.clone();
        corpus_terms.extend(context_keywords(&basis, context));
        MaterialSearchPlan {
            web_query: web_query(&basis, &query_terms, year_from, year_to),
            display_query: query_terms
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(" / "),
            corpus_query: dedupe(corpus_terms).join(" "),
            query_terms,
            year_from,
            year_to,
            strict_year_label,
            max_results: self.max_results,
            topic_context: (!topic_context.is_empty()).then_some(topic_context),
        }
    }

    pub fn build_reply(
        &self,
        plan: &MaterialSearchPlan,
        knowledge: &KnowledgeBundle,
        web_enabled: bool,
    ) -> String {
        let course = knowledge
            .hits
            .iter()
            .filter(|hit| matches!(hit.provider.as_str(), "corpus" | "course_corpus"))
            .collect::<Vec<_>>();
        let literature = knowledge
            .hits
            .iter()
            .filter(|hit| {
                !matches!(
                    hit.provider.as_str(),
                    "corpus"
                        | "course_corpus"
                        | "session_document"
                        | "web"
                        | "searxng"
                        | "bing"
                        | "brave"
                )
            })
            .collect::<Vec<_>>();
        let web = knowledge
            .hits
            .iter()
            .filter(|hit| matches!(hit.provider.as_str(), "web" | "searxng" | "bing" | "brave"))
            .collect::<Vec<_>>();
        [
            corpus_section(&course),
            online_section(plan, &literature, &web, knowledge, web_enabled),
            usage_section(plan, !literature.is_empty() || !web.is_empty()),
            next_step_section(plan),
        ]
        .join("\n\n")
    }
}

fn query_terms(text: &str, context: &WritingContext) -> Vec<String> {
    let lowered = text.to_lowercase();
    let mut terms = Vec::new();
    if contains_any(
        text,
        &["小组合作", "分工", "搭便车", "隐形乘客", "社会惰化", "团队"],
    ) {
        terms.extend(
            [
                "social loafing group work free rider",
                "free riding student group projects",
                "peer assessment group work free rider",
            ]
            .into_iter()
            .map(str::to_owned),
        );
    }
    if contains_any(text, &["搭子", "朋友", "友谊", "弱连接"]) {
        terms.extend(
            [
                "weak ties strong ties friendship college students",
                "social support peer relationships young adults",
                "social exchange theory friendship peer relationships",
            ]
            .into_iter()
            .map(str::to_owned),
        );
    }
    if contains_any(&lowered, &["ai", "aigc", "chatgpt"]) || text.contains("人工智能") {
        terms.extend(
            [
                "generative AI academic writing students",
                "AI writing higher education academic integrity",
            ]
            .into_iter()
            .map(str::to_owned),
        );
    }
    let keywords = context_keywords(text, context);
    if !keywords.is_empty() {
        terms.push(keywords.join(" "));
    }
    if terms.is_empty() {
        let cleaned = clean_query_text(text);
        terms.push(if cleaned.is_empty() {
            "academic writing student research".to_owned()
        } else {
            cleaned
        });
    }
    let mut terms = dedupe(terms);
    terms.truncate(4);
    terms
}

fn context_keywords(text: &str, context: &WritingContext) -> Vec<String> {
    let basis = [
        Some(text),
        context.topic.as_deref(),
        context.selected_direction.as_deref(),
        context.research_question.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ");
    let mut keywords = Vec::new();
    for (needle, keyword) in [
        ("搭子", "搭子"),
        ("朋友", "朋友关系"),
        ("小组合作", "小组合作"),
        ("分工", "分工不均"),
        ("搭便车", "搭便车"),
        ("团队", "团队合作"),
        ("社会惰化", "社会惰化"),
        ("友谊", "友谊"),
        ("弱连接", "弱连接"),
        ("弱关系", "弱连接"),
        ("强连接", "强连接"),
        ("社会交换", "社会交换理论"),
        ("社会网络", "社会网络理论"),
        ("情感支持", "情感支持"),
        ("同伴关系", "同伴关系"),
        ("青年社交", "青年社交"),
        ("大学生", "大学生"),
        ("AI", "AI写作"),
        ("人工智能", "AI写作"),
        ("学术自我效能", "学术自我效能"),
        ("教育", "教育"),
        ("精英", "精英教育"),
        ("博弈", "博弈论"),
    ] {
        if basis.contains(needle) && !keywords.iter().any(|item| item == keyword) {
            keywords.push(keyword.to_owned());
        }
    }
    keywords.truncate(5);
    keywords
}

fn corpus_section(results: &[&SearchHit]) -> String {
    if results.is_empty() {
        return "【课程材料】\n这次没有在已接入课程语料里命中足够明确的片段。".to_owned();
    }
    let mut seen = BTreeSet::new();
    let mut items = Vec::new();
    for result in results.iter().take(5) {
        let summary = summarize_corpus_item(result);
        if seen.insert(summary.clone()) {
            items.push(format!("{}. {summary}", items.len() + 1));
        }
        if items.len() == 3 {
            break;
        }
    }
    format!("【课程材料】\n{}", items.join("\n\n"))
}

fn online_section(
    plan: &MaterialSearchPlan,
    literature: &[&SearchHit],
    web: &[&SearchHit],
    knowledge: &KnowledgeBundle,
    web_enabled: bool,
) -> String {
    if !web_enabled {
        return "【联网文献】\n这次只查了本地课程语料，没有开启联网检索。要查 Semantic Scholar / Crossref / OpenAlex 和通用网页资料，需要打开前端“联网”按钮。".to_owned();
    }
    let mut blocks = Vec::new();
    if !literature.is_empty() {
        let mut heading = "学术库候选".to_owned();
        if let Some(label) = &plan.strict_year_label {
            heading.push_str(&format!("（严格按 {label} 返回，不混入旧文献）"));
        }
        let lines = literature
            .iter()
            .enumerate()
            .map(|(index, hit)| {
                let authors = if hit.authors.is_empty() {
                    "作者未知".to_owned()
                } else {
                    hit.authors.join(", ")
                };
                let title = markdown_link(hit);
                let year = hit
                    .year
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "年份未知".to_owned());
                let source = hit.provider.as_str();
                let abstract_hint = if !hit.text.trim().is_empty() {
                    format!("\n摘要线索：{}...", truncate_chars(&hit.text, 180))
                } else {
                    String::new()
                };
                format!(
                    "{}. {title} ({year}) - {authors}；来源：{source}{abstract_hint}",
                    index + 1
                )
            })
            .collect::<Vec<_>>();
        blocks.push(format!("{heading}：\n{}", lines.join("\n\n")));
    } else if let Some(error) = failure_message(knowledge, |provider| {
        !matches!(provider, "web" | "searxng" | "bing" | "brave")
    }) {
        blocks.push(format!("学术库候选：联网检索失败（学术库）：{error}。这不是“没有文献”，只是在线源这次没有成功返回；不要据此编造题名或 DOI。"));
    } else if let Some(label) = &plan.strict_year_label {
        blocks.push(format!("学术库候选：严格按 {label} 检索，没有命中足够相关的公开学术结果。可以下一步放宽到 2020 年以来，或改用英文关键词继续搜。"));
    } else {
        blocks.push(
            "学术库候选：联网学术库没有返回足够相关的候选结果，可以换英文关键词再搜。".to_owned(),
        );
    }
    if !web.is_empty() {
        let lines = web
            .iter()
            .enumerate()
            .map(|(index, hit)| {
                let source = if !hit.provider.is_empty() {
                    format!("；来源：{}", hit.provider)
                } else {
                    String::new()
                };
                let snippet = if !hit.text.trim().is_empty() {
                    format!("\n线索：{}...", truncate_chars(&hit.text, 180))
                } else {
                    String::new()
                };
                format!("{}. {}{source}{snippet}", index + 1, markdown_link(hit))
            })
            .collect::<Vec<_>>();
        let note = plan
            .strict_year_label
            .as_ref()
            .map(|_| "（网页结果按关键词返回，年份需要打开原文核对）")
            .unwrap_or("");
        blocks.push(format!("网页资料候选{note}：\n{}", lines.join("\n\n")));
    } else if let Some(error) = failure_message(knowledge, |provider| {
        matches!(provider, "web" | "searxng" | "bing" | "brave")
    }) {
        blocks.push(format!("网页资料候选：通用网页搜索失败：{error}。"));
    } else {
        blocks.push("网页资料候选：没有返回足够相关的网页结果。".to_owned());
    }
    format!("【联网文献】\n{}", blocks.join("\n\n"))
}

fn failure_message(knowledge: &KnowledgeBundle, include: impl Fn(&str) -> bool) -> Option<String> {
    knowledge
        .metadata
        .provider_failures
        .iter()
        .filter(|failure| include(&failure.provider))
        .map(|failure| format!("{}: {}", failure.provider, failure.message))
        .next()
}

fn usage_section(plan: &MaterialSearchPlan, has_online_results: bool) -> String {
    if has_online_results {
        "【怎么用】\n先不要堆文献名。把候选材料分成三类：概念界定、产生机制、方法设计。学术库结果优先看摘要和引用，网页资料只用来找现象、数据或案例线索；和题目最贴近的 2-3 条才进入正文。".to_owned()
    } else {
        format!(
            "【怎么用】\n先用课程材料确定问题框架，再用英文关键词补联网文献。这一轮关键词是：{}。",
            plan.display_query
        )
    }
}

fn next_step_section(plan: &MaterialSearchPlan) -> String {
    if plan.strict_year_label.is_some() {
        "【下一步筛选任务】\n先检查严格年份结果是否真的讨论你的研究问题。如果结果太少，再明确告诉我“放宽到 2020-2026”，我会把旧文献单独放在放宽范围里。".to_owned()
    } else {
        "【下一步筛选任务】\n从上面选 1 篇理论入口和 2 篇实证研究。如果你要更新的研究，只要再说具体年份范围。".to_owned()
    }
}

fn summarize_corpus_item(hit: &SearchHit) -> String {
    let text = hit.text.split_whitespace().collect::<Vec<_>>().join(" ");
    if contains_any(
        &text,
        &["隐形乘客", "搭便车", "团队生产", "小组合作", "分工"],
    ) {
        return "可直接参考：把“小组合作分工不均”界定为“隐形乘客/搭便车”问题，重点不是责备某个同学偷懒，而是解释任务边界、监督成本、评分规则和同伴压力怎样共同制造出力不均。\n可借用方法：用一次真实小组作业做开场，再做 3-5 个访谈或小问卷，比较“主动承担者、低参与者、组长/协调者”三类人的解释。".to_owned();
    }
    if contains_any(&text.to_lowercase(), &["ai", "chatgpt"]) || text.contains("生成式人工智能")
    {
        return "可直接参考：如果讨论 AI 进入小组合作，可以把 AI 当成新的协作变量，分析它是缓解分工压力，还是让责任归属更模糊。\n可借用方法：访谈同学在小组作业中如何分配 AI 使用任务，尤其关注“谁检查、谁署名、出错谁负责”。".to_owned();
    }
    if contains_any(&text, &["搭子", "朋友", "关系"]) {
        return "可直接参考：把日常关系现象转成研究问题时，先写清楚触发你困惑的具体场景，再区分功能、情感投入和责任期待，不要只停在网络热词介绍。\n可借用方法：先列 3 个具体例子，再问每个例子里双方一起做什么、是否倾诉重要事情、关系中断时有没有亏欠感。".to_owned();
    }
    if contains_any(&text, &["问卷", "访谈", "文本分析", "研究方法", "文献综述"]) {
        return "可直接参考：把材料收集设计成“小而清楚”的组合，不要一上来做很大的问卷。可以先用 3-5 个访谈找机制，再用小问卷验证哪些解释最常见。\n可借用方法：正文里把材料分成三类：个人经历或访谈负责提出问题，问卷负责显示分布，文献负责给概念和解释框架。".to_owned();
    }
    if contains_any(&text, &["博弈", "囚徒困境", "胆小鬼博弈", "策略"]) {
        return "可直接参考：如果你想解释小组里为什么没人愿意多干活，可以用“策略选择”而不是“道德评价”来写：每个人都在估算自己多做、少做、等待别人做的成本和收益。\n可借用方法：把一次分工冲突拆成“任务怎么分、谁先表态、谁承担后果、评分规则怎么影响选择”。".to_owned();
    }
    "可直接参考：这条课程材料适合用来检查题目是否有清楚的动机、对象和材料边界。你可以先把题目改写成“我观察到什么现象、我想解释哪个机制、我能拿到什么材料”。\n可借用方法：先写一个真实场景，再列两个可能解释和一个反例，用它们判断题目是否足够聚焦。".to_owned()
}

fn markdown_link(hit: &SearchHit) -> String {
    let title = hit.title.replace('[', "\\[").replace(']', "\\]");
    hit.url
        .as_deref()
        .or(hit.doi.as_deref())
        .or_else(|| {
            hit.source
                .starts_with("http")
                .then_some(hit.source.as_str())
        })
        .map(|link| format!("[{title}]({link})"))
        .unwrap_or(title)
}

fn web_query(
    text: &str,
    query_terms: &[String],
    year_from: Option<i32>,
    year_to: Option<i32>,
) -> String {
    let year = match (year_from, year_to) {
        (Some(start), Some(end)) => format!(" {start} {end}"),
        (Some(start), None) => format!(" {start}"),
        _ => String::new(),
    };
    let base = if contains_any(
        text,
        &["小组合作", "分工", "搭便车", "隐形乘客", "社会惰化", "团队"],
    ) {
        "小组合作 分工不均 搭便车 社会惰化 free rider social loafing".to_owned()
    } else if contains_any(text, &["搭子", "朋友", "友谊", "弱连接"]) {
        "搭子 朋友关系 弱连接 大学生 青年社交 friendship weak ties".to_owned()
    } else if contains_any(&text.to_lowercase(), &["ai", "aigc", "chatgpt"])
        || text.contains("人工智能")
    {
        "生成式人工智能 学术写作 大学生 academic writing AI students".to_owned()
    } else {
        query_terms
            .first()
            .cloned()
            .unwrap_or_else(|| "academic writing student research".to_owned())
    };
    format!("{base}{year}").trim().to_owned()
}

pub(crate) fn extract_year_range(message: &str) -> (Option<i32>, Option<i32>) {
    let years = message
        .split(|character: char| !character.is_ascii_digit())
        .filter(|value| value.len() == 4)
        .filter_map(|value| value.parse::<i32>().ok())
        .filter(|year| (1900..=2200).contains(year))
        .collect::<Vec<_>>();
    match years.as_slice() {
        [year, ..] if contains_any(message, &["后", "以后", "起"]) => (Some(*year), None),
        [year, ..] if contains_any(message, &["前", "以前", "截止"]) => (None, Some(*year)),
        [start, end, ..] => (Some(*start), Some(*end)),
        [year] => (Some(*year), Some(*year)),
        _ => (None, None),
    }
}

fn clean_query_text(text: &str) -> String {
    let mut cleaned = text.to_owned();
    for token in [
        "文献",
        "资料",
        "搜索",
        "联网",
        "找一些",
        "找一下",
        "有没有",
        "请",
        "帮我",
    ] {
        cleaned = cleaned.replace(token, " ");
    }
    cleaned
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(120)
        .collect()
}

fn dedupe(values: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    values
        .into_iter()
        .filter(|value| !value.is_empty() && seen.insert(value.clone()))
        .collect()
}

fn contains_any(text: &str, patterns: &[&str]) -> bool {
    patterns.iter().any(|pattern| text.contains(pattern))
}

fn truncate_chars(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

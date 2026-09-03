#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DomainBoundaryDecision {
    pub redirect: bool,
    pub reply: Option<String>,
    pub reason: Option<&'static str>,
}

const COURSE_TERMS: &[&str] = &[
    "写作",
    "写一篇",
    "作文",
    "文章",
    "论文",
    "选题",
    "提纲",
    "观点",
    "论点",
    "论据",
    "论证",
    "段落",
    "修改",
    "表达",
    "沟通",
    "演讲",
    "汇报",
    "答辩",
    "访谈",
    "文献",
    "阅读",
    "课程",
    "课件",
    "互评",
    "研究",
    "材料",
    "剧本",
    "叙事",
];
const PERSONAL_ROLES: &[&str] = &[
    "奶奶",
    "爷爷",
    "外婆",
    "外公",
    "姥姥",
    "姥爷",
    "妈妈",
    "爸爸",
    "母亲",
    "父亲",
    "妻子",
    "丈夫",
    "男朋友",
    "女朋友",
    "恋人",
    "过世的亲人",
];
const ROLEPLAY_ACTIONS: &[&str] = &["扮演", "假装", "冒充", "你就是", "当我的", "成为我的"];
const STOP_ROLEPLAY_ACTIONS: &[&str] = &["停止扮演", "不要扮演", "别扮演", "不再扮演", "退出角色"];
const EXPLICIT_OFF_TOPIC_TERMS: &[&str] = &[
    "天气怎么样",
    "天气预报",
    "星座运势",
    "给我算命",
    "游戏攻略",
    "旅游路线",
    "股票推荐",
    "彩票号码",
    "体育比分",
    "电影推荐",
    "菜谱",
    "陪我闲聊",
    "陪我聊天",
    "唱首歌",
    "讲个笑话",
];

pub(crate) fn evaluate_domain_boundary(
    message: &str,
    general_fallback: bool,
) -> DomainBoundaryDecision {
    let text = message.split_whitespace().collect::<Vec<_>>().join(" ");
    let lowered = text.to_lowercase();
    if matches!(
        lowered.as_str(),
        "你好" | "您好" | "嗨" | "hi" | "hello" | "在吗"
    ) {
        return redirect(
            "你好，我是写作与沟通智能体。你可以直接说现在卡在哪里，比如选题没灵感、文章结构不清、材料不足，或者不知道怎样表达，我会接着帮你。",
            "greeting_return_to_course_scope",
        );
    }
    let role = PERSONAL_ROLES.iter().find(|role| lowered.contains(**role));
    if let Some(role) = role {
        if STOP_ROLEPLAY_ACTIONS
            .iter()
            .any(|action| lowered.contains(action))
        {
            return redirect(
                "明白，我已经停止角色扮演。刚才的问题是把你的话继续套进了人物身份，没有直接回应你真正提出的要求。现在我会明确以写作与沟通智能体的身份回答，并把对话重新聚焦到你的写作、表达、阅读或沟通任务。",
                "stop_personal_identity_roleplay",
            );
        }
        let impersonation = ROLEPLAY_ACTIONS
            .iter()
            .any(|action| lowered.contains(action));
        let addressed = lowered.starts_with(&format!("{role}{role}"))
            || ["，", ",", "：", ":"]
                .iter()
                .any(|separator| lowered.starts_with(&format!("{role}{separator}")));
        if impersonation || addressed {
            let boundary = if impersonation {
                format!("我不能成为或冒充你的{role}，也不会编造我们共同经历过的记忆。")
            } else {
                format!("我不会以你的{role}身份继续回答，也不会编造我们共同经历过的记忆。")
            };
            return redirect(
                format!(
                    "{boundary}如果你愿意，我可以在写作与沟通的范围内，帮你把对这位亲人的思念整理成人物叙事、回忆片段或访谈式文字。你可以先写下一个印象最深的真实场景。"
                ),
                "personal_identity_roleplay",
            );
        }
    }
    if COURSE_TERMS.iter().any(|term| lowered.contains(term)) {
        return allow();
    }
    if general_fallback
        || EXPLICIT_OFF_TOPIC_TERMS
            .iter()
            .any(|term| lowered.contains(term))
    {
        return redirect(
            "这个话题不属于写作与沟通课程的主要范围，我不继续把它展开成普通闲聊。你可以直接告诉我一个写作、表达、阅读或沟通上的任务；也可以把刚才的话题改成一个选题、叙事或观点分析。",
            "outside_writing_communication_scope",
        );
    }
    allow()
}

fn allow() -> DomainBoundaryDecision {
    DomainBoundaryDecision {
        redirect: false,
        reply: None,
        reason: None,
    }
}

fn redirect(reply: impl Into<String>, reason: &'static str) -> DomainBoundaryDecision {
    DomainBoundaryDecision {
        redirect: true,
        reply: Some(reply.into()),
        reason: Some(reason),
    }
}

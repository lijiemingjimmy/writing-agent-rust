export const skillNames: Record<string, string> = {
  writing_feedback: "写作反馈", novelty_eval: "选题梳理", socratic_review: "追问推进",
  material_search: "资料检索", literature_reading: "文献阅读", ppt_qa: "课件问答",
  peer_review_training: "互评训练", research_question_evaluator: "研究问题",
  theory_fit_checker: "理论适配", method_feasibility_checker: "方法检查",
  draft_diagnosis: "初稿诊断", course_policy_qa: "课程规则",
  ai_use_boundary_qa: "AI 边界", academic_norm_check: "学术规范"
};

export function stageText(stage?: string | null) {
  return ({ topic: "选题", research_question: "研究问题", theory: "理论", literature: "文献",
    method: "方法", draft_argument: "论证/初稿", course_policy: "课程规则",
    academic_norm: "学术规范", unknown: "自然聊天" } as Record<string, string>)[stage || ""] || stage || "未识别";
}

export function formatDate(value?: string | null) {
  if (!value) return "-";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString("zh-CN", { month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" });
}

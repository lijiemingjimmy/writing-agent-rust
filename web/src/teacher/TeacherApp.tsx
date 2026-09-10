import { FormEvent, useEffect, useState } from "react";
import { fetchModelSettings, analyzeTeacherJson, askTeacherArchive, deleteTeacherStudentSession, downloadTeacherExport, fetchTeacherClassInsights, fetchTeacherClassSummary, fetchTeacherStats, fetchTeacherStudentDetail, fetchTeacherStudentSummary, fetchTeacherStudents, summarizeTeacher, teacherAccessStorageKey } from "../api";
import { TeacherLayout } from "./TeacherLayout";
import { pathFromLocation } from "../locationPath.mjs";
import { parseTeacherRoute } from "./teacherRoutes.mjs";
import { StudentDrawer } from "./components/TeacherViews";
import { ConversationsPage, ImportPage, InsightsPage, OverviewPage, SettingsPage, StudentsPage } from "./pages/TeacherPages";

export function TeacherApp() {
  const saved=window.localStorage.getItem(teacherAccessStorageKey)||"";
  const [tokenInput,setTokenInput]=useState(saved),[authenticated,setAuthenticated]=useState(false),[authLoading,setAuthLoading]=useState(Boolean(saved)),[authError,setAuthError]=useState("");
  const [route,setRoute]=useState(()=>parseTeacherRoute(currentPath())||"overview");
  const [stats,setStats]=useState<any>(null),[students,setStudents]=useState<any[]>([]),[insights,setInsights]=useState<any>(null);
  const [sharedModel,setSharedModel]=useState<any>(null),[modelError,setModelError]=useState("");
  const [globalError,setGlobalError]=useState(""),[studentError,setStudentError]=useState("");
  const [teachingSummary,setTeachingSummary]=useState(""),[classSummary,setClassSummary]=useState(""),[loadingTeaching,setLoadingTeaching]=useState(false),[loadingClass,setLoadingClass]=useState(false);
  const [archiveQuestion,setArchiveQuestion]=useState(""),[archiveAnswer,setArchiveAnswer]=useState(""),[archiveEvidence,setArchiveEvidence]=useState<any[]>([]),[archiveError,setArchiveError]=useState(""),[askingArchive,setAskingArchive]=useState(false);
  const [importAnalysis,setImportAnalysis]=useState<any>(null),[importing,setImporting]=useState(false),[importError,setImportError]=useState("");
  const [drawerOpen,setDrawerOpen]=useState(false),[drawerTab,setDrawerTab]=useState("summary"),[drawerSessionId,setDrawerSessionId]=useState<string|null>(null),[drawerSummary,setDrawerSummary]=useState<any>(null),[drawerDetail,setDrawerDetail]=useState<any>(null),[drawerLoading,setDrawerLoading]=useState(false),[deleting,setDeleting]=useState<string|null>(null);

  useEffect(()=>{const handler=()=>setRoute(parseTeacherRoute(currentPath())||"overview");window.addEventListener("hashchange",handler);window.addEventListener("popstate",handler);return()=>{window.removeEventListener("hashchange",handler);window.removeEventListener("popstate",handler);};},[]);
  useEffect(()=>{if(saved) void validateToken();},[]);
  useEffect(()=>{if(!authenticated)return;if(!students.length&&(route==="overview"||route==="students"))void loadStudents();if(route==="insights"&&!insights)void loadInsights();},[authenticated,route]);
  useEffect(()=>{const close=(e:KeyboardEvent)=>{if(e.key==="Escape")setDrawerOpen(false)};window.addEventListener("keydown",close);return()=>window.removeEventListener("keydown",close);},[]);

  useEffect(()=>{if(!authenticated)return;const refresh=()=>void loadSharedModel();refresh();window.addEventListener("focus",refresh);return()=>window.removeEventListener("focus",refresh);},[authenticated,route]);
  async function loadSharedModel(){try{setSharedModel(await fetchModelSettings());setModelError("");}catch(e){setModelError(errorText(e));}}

  async function validateToken(){setAuthLoading(true);try{const result=await fetchTeacherStats();setStats(result);setAuthenticated(true);setAuthError("");setGlobalError("");}catch(e){setAuthenticated(false);setAuthError(errorText(e));window.localStorage.removeItem(teacherAccessStorageKey);}finally{setAuthLoading(false);}}
  async function submitAccess(e:FormEvent){e.preventDefault();const value=tokenInput.trim();if(!value){setAuthError("请输入教师访问码。");return;}window.localStorage.setItem(teacherAccessStorageKey,value);await validateToken();}
  function logout(){window.localStorage.removeItem(teacherAccessStorageKey);setAuthenticated(false);setTokenInput("");setStats(null);setStudents([]);setInsights(null);}
  async function loadStats(){try{setStats(await fetchTeacherStats());setGlobalError("");}catch(e){setGlobalError(errorText(e));}}
  async function loadStudents(){try{setStudents((await fetchTeacherStudents()).students||[]);setStudentError("");}catch(e){setStudentError(errorText(e));}}
  async function loadInsights(){try{setInsights(await fetchTeacherClassInsights());setGlobalError("");}catch(e){setGlobalError(errorText(e));}}
  async function refreshCurrent(){if(route==="students")await loadStudents();else if(route==="insights")await loadInsights();else await loadStats();}
  async function summarizeTeaching(){setLoadingTeaching(true);try{setTeachingSummary((await summarizeTeacher(500)).summary);setGlobalError("");}catch(e){setGlobalError(errorText(e));}finally{setLoadingTeaching(false);}}
  async function summarizeClass(){setLoadingClass(true);try{setClassSummary((await fetchTeacherClassSummary()).markdown);setGlobalError("");}catch(e){setGlobalError(errorText(e));}finally{setLoadingClass(false);}}
  async function askArchive(question=archiveQuestion){const q=question.trim();if(!q||askingArchive)return;setArchiveQuestion(q);setAskingArchive(true);setArchiveAnswer("");setArchiveEvidence([]);setArchiveError("");void loadSharedModel();try{const r=await askTeacherArchive(q,300);setArchiveAnswer(r.answer);setArchiveEvidence(r.evidence||[]);setArchiveError("");}catch(e){setArchiveError(errorText(e));}finally{setAskingArchive(false);}}
  async function analyzeUpload(file:File|null){if(!file)return;setImporting(true);setImportError("");try{setImportAnalysis(await analyzeTeacherJson(file));}catch(e){setImportError(errorText(e));}finally{setImporting(false);}}
  async function openStudent(id:string,tab:string){if(id!==drawerSessionId){setDrawerSummary(null);setDrawerDetail(null);setDrawerSessionId(id);}setDrawerOpen(true);setDrawerTab(tab);setDrawerLoading(true);try{if(tab==="summary")setDrawerSummary(await fetchTeacherStudentSummary(id));else setDrawerDetail(await fetchTeacherStudentDetail(id));setStudentError("");}catch(e){setStudentError(errorText(e));}finally{setDrawerLoading(false);}}
  async function changeDrawerTab(tab:string){setDrawerTab(tab);if(!drawerSessionId)return;if(tab==="summary"&&!drawerSummary)await openStudent(drawerSessionId,tab);if(tab==="detail"&&!drawerDetail)await openStudent(drawerSessionId,tab);}
  async function deleteStudent(id:string){const item=students.find(x=>x.session_id===id);const label=item?.student_name||item?.student_id||`会话 ${id.slice(0,8)}`;if(!window.confirm(`确定删除 ${label} 的这段对话吗？删除后无法从后台恢复。`))return;setDeleting(id);try{await deleteTeacherStudentSession(id);setDrawerOpen(false);await Promise.all([loadStudents(),loadStats()]);}catch(e){setStudentError(errorText(e));}finally{setDeleting(null);}}
  async function exportData(format:"csv"|"json"){try{await downloadTeacherExport(format);setGlobalError("");}catch(e){setGlobalError(errorText(e));}}

  if(authLoading)return <main className="teacher-access-page"><p>正在验证教师访问...</p></main>;
  if(!authenticated)return <main className="teacher-access-page"><section className="teacher-access-card"><h1>教师端访问</h1><form className="teacher-access-form" onSubmit={submitAccess}><input value={tokenInput} onChange={e=>setTokenInput(e.target.value)} type="password" placeholder="教师访问码" autoFocus/><button>进入教师端</button></form>{authError&&<p className="teacher-error" role="alert">{authError}</p>}</section></main>;
  const model={sharedModel,modelError,loadSharedModel,stats,students,insights,globalError,studentError,teachingSummary,classSummary,loadingTeaching,loadingClass,summarizeTeaching,summarizeClass,archiveQuestion,setArchiveQuestion,archiveAnswer,archiveEvidence,archiveError,askingArchive,askArchive,importAnalysis,importing,importError,analyzeUpload,loadStudents,loadInsights,openStudent,deleteStudent,deleting,logout,exportData};
  const pages:any={overview:OverviewPage,students:StudentsPage,conversations:ConversationsPage,insights:InsightsPage,import:ImportPage,settings:SettingsPage};const Page=pages[route]||OverviewPage;
  return <TeacherLayout route={route} onRefresh={refreshCurrent} onExport={exportData} onLogout={logout}>{globalError&&<p className="teacher-error">{globalError}</p>}<Page model={model}/><StudentDrawer open={drawerOpen} tab={drawerTab} onTab={changeDrawerTab} onClose={()=>setDrawerOpen(false)} summary={drawerSummary} detail={drawerDetail} loading={drawerLoading}/></TeacherLayout>;
}

function currentPath(){return pathFromLocation(window.location.hash.replace(/^#/,""),window.location.pathname,import.meta.env.BASE_URL||"/");}
function errorText(e:unknown){return e instanceof Error?e.message:String(e);}

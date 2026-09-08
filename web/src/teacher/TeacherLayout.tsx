import { appPath } from "../paths";
import { teacherRoutes } from "./teacherRoutes.mjs";

export function TeacherLayout({ route, children, onRefresh, onExport, onLogout }: any) {
  return <main className="teacher-workbench">
    <header className="teacher-topbar">
      <div className="teacher-title"><strong>写作与沟通教师端</strong></div>
      <div className="teacher-global-actions"><span className="backend-ok">后端已连接</span><button onClick={onRefresh}>刷新</button><button onClick={()=>onExport("csv")}>导出 CSV</button><button onClick={()=>onExport("json")}>导出 JSON</button><a href={appPath("/student")}>学生端</a><button onClick={onLogout}>退出</button></div>
    </header>
    <nav className="teacher-tabs" aria-label="教师端页面">{teacherRoutes.map((item: any) => <a key={item.id} className={route === item.id ? "active" : ""} href={appPath(`/teacher/${item.id}`)}>{item.label}</a>)}</nav>
    <div className="teacher-content">{children}</div>
  </main>;
}

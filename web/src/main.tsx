import { lazy, Suspense, useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { StudentChat } from "./pages/StudentChat";
import { pathFromLocation } from "./locationPath.mjs";
import "./styles.css";

const TeacherDashboard = lazy(() =>
  import("./pages/TeacherDashboard").then((module) => ({ default: module.TeacherDashboard }))
);

function currentPath() {
  return pathFromLocation(
    window.location.hash.replace(/^#/, ""),
    window.location.pathname,
    import.meta.env.BASE_URL || "/"
  );
}

function App() {
  const [path, setPath] = useState(currentPath);

  useEffect(() => {
    const update = () => setPath(currentPath());
    window.addEventListener("hashchange", update);
    window.addEventListener("popstate", update);
    return () => {
      window.removeEventListener("hashchange", update);
      window.removeEventListener("popstate", update);
    };
  }, []);

  if (path.startsWith("/teacher")) {
    return <Suspense fallback={<main className="teacher-access-page"><p>正在加载教师端...</p></main>}><TeacherDashboard /></Suspense>;
  }
  return <StudentChat />;
}

createRoot(document.getElementById("root")!).render(<App />);

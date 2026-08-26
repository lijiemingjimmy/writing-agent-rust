import { createRoot } from "react-dom/client";
import { StudentChat } from "./pages/StudentChat";
import "./styles.css";

function App() {
  return <StudentChat />;
}

createRoot(document.getElementById("root")!).render(<App />);

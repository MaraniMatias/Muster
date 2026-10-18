import { createApp } from "vue";
import App from "./App.vue";
import { installContextMenu } from "./lib/context-menu";
import { preloadTerminalFonts } from "./lib/muster-terminal";
import { installFrontendErrorHandlers } from "./lib/diagnostics";
import "./muster.css";
import "./style.css";

installContextMenu();
// The terminal cannot draw its grid against a face it has not parsed, and these two are the
// largest assets the app ships. Asking at startup rather than at the first panel means the parse
// overlaps the launch instead of standing between the click and the prompt.
void preloadTerminalFonts();
const app = createApp(App);
installFrontendErrorHandlers(app);
app.mount("#app");

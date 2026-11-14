export { LightToast } from "./feedback";
export { AppDialogStack } from "./dialog-stack";
export { MainLayout } from "./layout";
export { UpdateDialog } from "./update";
// F1.5: view components are intentionally NOT re-exported here. App.tsx only
// needs the shell components above; views load exclusively via React.lazy in
// layout.tsx. Re-exporting them here pulled every view chunk into the entry's
// static graph (rolldown keeps unused barrel re-exports), defeating the
// per-view code splitting.

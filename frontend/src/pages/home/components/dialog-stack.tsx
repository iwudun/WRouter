import { lazy, Suspense, type ComponentProps, type ReactElement } from "react";
import { AnimatePresence } from "motion/react";
import { DialogStackLayer } from "@/components/ui/dialog";
import { UpdateDialog } from "./update";

// F1.5: dialogs that live inside the lazy view directories load via
// React.lazy. The pre-F1 static imports pulled the whole view chunks
// (api-keys / providers) into the entry's static graph and defeated
// the per-view code splitting. Each dialog renders only while its trigger
// state is set, so the (null) Suspense fallback is never visible in practice.
const AddApiKeyDialog = lazy(() => import("./api-keys").then((m) => ({ default: m.AddApiKeyDialog })));
const ApiKeyCreatedDialog = lazy(() => import("./api-keys").then((m) => ({ default: m.ApiKeyCreatedDialog })));
const EditApiKeyDialog = lazy(() => import("./api-keys").then((m) => ({ default: m.EditApiKeyDialog })));
const DeleteProviderDialog = lazy(() => import("./providers").then((m) => ({ default: m.DeleteProviderDialog })));

function DialogFallback() {
  return null;
}

export function AppDialogStack({
  apiKeyAdd,
  apiKeyCreated,
  apiKeyEdit,
  providerDelete,
  update
}: {
  apiKeyAdd?: ComponentProps<typeof AddApiKeyDialog>;
  apiKeyCreated?: ComponentProps<typeof ApiKeyCreatedDialog>;
  apiKeyEdit?: ComponentProps<typeof EditApiKeyDialog>;
  providerDelete?: ComponentProps<typeof DeleteProviderDialog>;
  update?: ComponentProps<typeof UpdateDialog>;
}) {
  const dialogs = [
    apiKeyAdd ? { key: "api-key-add", node: <AddApiKeyDialog {...apiKeyAdd} /> } : null,
    apiKeyCreated ? { key: "api-key-created", node: <ApiKeyCreatedDialog {...apiKeyCreated} /> } : null,
    apiKeyEdit ? { key: "api-key-edit", node: <EditApiKeyDialog {...apiKeyEdit} /> } : null,
    providerDelete ? { key: "provider-delete", node: <DeleteProviderDialog {...providerDelete} /> } : null,
    update ? { key: "update", node: <UpdateDialog {...update} /> } : null
  ].filter((dialog): dialog is { key: string; node: ReactElement } => Boolean(dialog));

  return (
    <AnimatePresence initial={false}>
      {dialogs.map((dialog, index) => (
        <DialogStackLayer depth={dialogs.length - index - 1} key={dialog.key}>
          <Suspense fallback={<DialogFallback />}>{dialog.node}</Suspense>
        </DialogStackLayer>
      ))}
    </AnimatePresence>
  );
}

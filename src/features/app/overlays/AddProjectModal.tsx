import type { RefObject } from "react";
import type { SettingsSectionId } from "@/lib/settingsCatalog";
import type { DragZone } from "@/lib/dragZone";
import { pathBasename } from "@/lib/attachments";
import { projectPathPreview } from "@/features/app/models";
import type { AddProjectIntent } from "@/hooks/useProjectDialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Spinner } from "@/components/ui/spinner";
import { GlassModal } from "@/components/GlassModal";
import { IconFolder, IconFolderPlus } from "@/components/icons";
import type { SetState, Translator } from "./types";

export interface AddProjectModalProps {
  tr: Translator;
  intent: AddProjectIntent | null;
  name: string;
  setName: SetState<string>;
  path: string;
  busy: boolean;
  error: string | null;
  nameRef: RefObject<HTMLInputElement | null>;
  dropRef: RefObject<HTMLButtonElement | null>;
  returnFocusRef: RefObject<HTMLElement | null>;
  nameEditedRef: RefObject<boolean>;
  setError: SetState<string | null>;
  dragZone: DragZone;
  projectDirectory: string;
  close: () => void;
  submit: () => void | Promise<void>;
  pickDirectory: () => void | Promise<void>;
  reset: () => void;
  navigateSettings: (section?: SettingsSectionId) => void;
}

export function AddProjectModal({
  tr,
  intent,
  name,
  setName,
  path,
  busy,
  error,
  nameRef,
  dropRef,
  returnFocusRef,
  nameEditedRef,
  setError,
  dragZone,
  projectDirectory,
  close,
  submit,
  pickDirectory,
  reset,
  navigateSettings,
}: AddProjectModalProps) {
  const openProjectDirectorySettings = () => {
    reset();
    navigateSettings("general");
  };

  return (
    <GlassModal
      open={intent !== null}
      onClose={close}
      title={tr("addProject.title")}
      titleId="add-project-title"
      size="lg"
      className="add-project-modal"
      overlayClassName="add-project-overlay"
      closeLabel={tr("common.close")}
      closeOnOverlay={!busy}
      showClose={!busy}
      wrapBody
      returnFocusRef={returnFocusRef}
      footer={
        <>
          <Button
            type="button"
            className="btn btn--ghost"
            disabled={busy}
            onClick={close}
          >
            {tr("common.cancel")}
          </Button>
          <Button
            type="submit"
            form="add-project-form"
            className="btn btn--solid"
            disabled={busy}
          >
            {busy ? <Spinner size={14} /> : null}
            {busy ? tr("addProject.adding") : tr("addProject.submit")}
          </Button>
        </>
      }
    >
      <form
        id="add-project-form"
        className="add-project-form"
        onSubmit={(event) => {
          event.preventDefault();
          void submit();
        }}
      >
        <div className="add-project-field">
          <Label htmlFor="add-project-name" className="prov-field__label">
            {tr("addProject.name")}
          </Label>
          <div className="add-project-name-control">
            <IconFolder size={17} />
            <Input
              ref={nameRef}
              id="add-project-name"
              className="settings-input"
              value={name}
              placeholder={tr("addProject.namePlaceholder")}
              maxLength={120}
              autoComplete="off"
              data-modal-autofocus
              readOnly={busy}
              aria-invalid={
                error === tr("addProject.nameRequired") || undefined
              }
              aria-describedby={error ? "add-project-error" : undefined}
              onChange={(event) => {
                const effectiveNameEditedRef = nameEditedRef;
                effectiveNameEditedRef.current = true;
                setName(event.target.value);
                setError(null);
              }}
            />
          </div>
        </div>

        <div className="add-project-field">
          <Label htmlFor="add-project-source" className="prov-field__label">
            {tr("addProject.sourceFolder")}
          </Label>
          <Button
            ref={dropRef}
            id="add-project-source"
            type="button"
            className={
              "cpm__action add-project-drop" +
              (dragZone === "project" ? " is-active" : "")
            }
            disabled={busy}
            onClick={() => void pickDirectory()}
            aria-label={
              path ? pathBasename(path) : tr("addProject.chooseFolder")
            }
          >
            <IconFolderPlus size={24} />
            <strong className="add-project-drop__title">
              {path ? pathBasename(path) : tr("addProject.chooseFolder")}
            </strong>
            {path ? (
              <span className="add-project-drop__path" title={path}>
                {path}
              </span>
            ) : null}
          </Button>
          {!path && projectDirectory ? (
            <div className="add-project-default-path settings-row__desc">
              <span>
                {tr("addProject.defaultLocation", {
                  path: projectPathPreview(
                    projectDirectory,
                    name.trim() || tr("addProject.namePlaceholder"),
                  ),
                })}
              </span>
              <Button
                type="button"
                className="add-project-default-path__settings btn btn--ghost btn--sm"
                onClick={openProjectDirectorySettings}
              >
                {tr("addProject.changeDefaultLocation")}
              </Button>
            </div>
          ) : null}
        </div>

        {error ? (
          <p
            id="add-project-error"
            className="ext-alert ext-alert--error"
            role="alert"
          >
            {error}
          </p>
        ) : null}
      </form>
    </GlassModal>
  );
}

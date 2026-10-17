import type { ContentPart, ProviderConnection } from '../generated/bindings';
import {
  setWorkspaceProvider,
  storeWorkspaceImage,
  updateWorkspaceSessionMcp,
  type PickedImage,
} from '../workspace/client';

/**
 * Model, MCP and image choices made in the Fleet terminal with no card selected,
 * applied to the workspace the next prompt creates. The empty provider means
 * the default a new workspace gets (the first enabled connection).
 */
export const newWorkspaceDraft = {
  providerConnectionId: '',
  mcpServerIds: [] as string[],
  disabledMcpServerIds: [] as string[],
};

/** The draft's connection while it is enabled, else the default: the first enabled one. */
export function draftConnectionId(connections: ProviderConnection[]): string | undefined {
  const enabled = connections.filter((c) => c.enabled);
  return (enabled.find((c) => c.id === newWorkspaceDraft.providerConnectionId) ?? enabled[0])?.id;
}

export async function applyNewWorkspaceDraft(workspaceId: string, connections: ProviderConnection[]): Promise<void> {
  const providerConnectionId = draftConnectionId(connections);
  if (providerConnectionId) await setWorkspaceProvider(workspaceId, providerConnectionId);
  await updateWorkspaceSessionMcp(workspaceId, newWorkspaceDraft.mcpServerIds, newWorkspaceDraft.disabledMcpServerIds);
}

/** Images attached to the new-workspace prompt: there is no image store until the workspace exists. */
const draftImages = new Map<string, PickedImage>();

/** An image part standing in for `image` until `storeDraftImages` stores it. */
export function draftImagePart(image: PickedImage): ContentPart {
  const id = crypto.randomUUID();
  draftImages.set(id, image);
  return { type: 'image', id, path: '', media_type: image.mediaType, filename: image.filename };
}

/**
 * Store the draft images among `parts` in the new workspace, in order. Kept
 * until `dropDraftImages`: a failed send keeps them attached for a retry,
 * which may create another workspace.
 */
export function storeDraftImages(workspaceId: string, parts: ContentPart[]): Promise<ContentPart[]> {
  return Promise.all(
    parts.map((part) => {
      const image = part.type === 'image' ? draftImages.get(part.id) : undefined;
      return image
        ? storeWorkspaceImage(workspaceId, image.dataBase64, image.mediaType, image.filename)
        : part;
    })
  );
}

/** Forget the draft images among `parts`, once the prompt carrying them is sent. */
export function dropDraftImages(parts: ContentPart[]): void {
  for (const part of parts) if (part.type === 'image') draftImages.delete(part.id);
}

import type { ContentPart } from '../generated/bindings';
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

export async function applyNewWorkspaceDraft(workspaceId: string): Promise<void> {
  const { providerConnectionId, mcpServerIds, disabledMcpServerIds } = newWorkspaceDraft;
  if (providerConnectionId) await setWorkspaceProvider(workspaceId, providerConnectionId);
  await updateWorkspaceSessionMcp(workspaceId, mcpServerIds, disabledMcpServerIds);
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
 * after storing: a failed send keeps them attached for a retry, which may
 * create another workspace.
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

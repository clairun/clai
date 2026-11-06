// A container engine runs containers as root, outside the network mode.
const CONTAINER_ENGINE_SOCKET = /(docker|podman|containerd)\.sock$/;

/**
 * What a grant on a Docker, Podman or containerd socket gives the agent, or
 * null for any other path. It informs only: such a grant is approved and
 * saved like any other.
 */
export const containerEngineWarning = (path: string): string | null =>
  CONTAINER_ENGINE_SOCKET.test(path.trim())
    ? 'Granting a Docker, Podman or containerd socket gives this agent full control of this computer: '
      + 'it can start a container that mounts any folder with root rights. '
      + "Containers also do not follow this agent's network mode."
    : null;

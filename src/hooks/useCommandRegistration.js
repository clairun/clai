import { useEffect, useMemo, useRef } from 'react';
import { useTabManager } from '../contexts/TabManagerContext';
import { useTabContext } from '../contexts/TabContext';

/**
 * Hook for command components to register their API with the CommandRegistry.
 *
 * Usage in a content component:
 * ```jsx
 * const Canvas = ({ command }) => {
 *   const [nodes, setNodes] = useState([]);
 *
 *   useCommandRegistration(command.id, () => ({
 *     type: 'canvas',
 *     addNode: (node) => setNodes(prev => [...prev, node]),
 *     getNodes: () => nodes,
 *   }), [nodes, setNodes]);
 *
 *   // ...
 * };
 * ```
 *
 * The API is registered when the component mounts and unregistered on unmount.
 * The API is re-registered whenever dependencies change.
 *
 * @param {string} commandId - The command's ID
 * @param {function} apiFactory - Factory function that returns the API object
 * @param {array} deps - Dependencies for the API factory (like useMemo)
 * @returns {object} The current API object
 */
export const useCommandRegistration = (commandId, apiFactory, deps = []) => {
  const { registerCommandApiInTab, unregisterCommandApiInTab } = useTabManager();
  const { tabId } = useTabContext();

  // Track if we've registered, to avoid double registration
  const registeredRef = useRef(false);
  const registeredTabIdRef = useRef(null);

  // Create the API object, memoized on dependencies

  const api = useMemo(apiFactory, deps);

  useEffect(() => {
    if (!commandId || !api || !tabId) {
      return;
    }

    // Register the API in the component's specific tab
    registerCommandApiInTab(tabId, commandId, api);
    registeredRef.current = true;
    registeredTabIdRef.current = tabId;

    // Unregister on unmount or when commandId/api changes
    return () => {
      if (registeredRef.current && registeredTabIdRef.current) {
        unregisterCommandApiInTab(registeredTabIdRef.current, commandId);
        registeredRef.current = false;
        registeredTabIdRef.current = null;
      }
    };
  }, [commandId, api, tabId, registerCommandApiInTab, unregisterCommandApiInTab]);

  return api;
};

export default useCommandRegistration;

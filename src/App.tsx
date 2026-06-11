import { useEffect } from 'react';
import { BrowserRouter as Router } from 'react-router-dom';
import AppRoutes from './Routes';
import { usePlatform } from './hooks/usePlatform';

function App() {
  const { os } = usePlatform();

  useEffect(() => {
    // Apply desktop platform class to the document root
    document.documentElement.setAttribute('data-platform', os);
    document.documentElement.setAttribute('data-device-type', 'desktop');

    // Also add as classes for easier CSS targeting
    document.documentElement.classList.add(`platform-${os}`);
    document.documentElement.classList.add('device-desktop');
  }, [os]);

  return (
    <Router>
      <AppRoutes />
    </Router>
  );
}

export default App;

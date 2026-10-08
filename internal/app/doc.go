// Package app assembles the application state root, the service layer
// (sessions, settings, project directory), the runtime bridge (event
// projection, turn assembly, permission dialogs), and the window shell —
// sidebar with the session list, header, chat timeline, composer — on top
// of the internal packages and mygo. The settings page is a later wave;
// the settings service bridge already lives here.
package app

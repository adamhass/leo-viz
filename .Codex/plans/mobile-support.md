# leo-viz Mobile Support Plan

## Goal

Make leo-viz usable on phones and tablets without reducing the desktop/presentation experience.

## Current State

The app already runs on mobile browsers, but the UI is desktop-oriented:

- Dense side panel controls assume wide screens.
- Many interactions rely on hover, which does not exist on touch devices.
- Floating windows can exceed the viewport or overlap the main view.
- Small buttons and drag values are hard to use with fingers.
- Presentation/demo mode works better than editing mode, but still has desktop chrome.

## Design Direction

Use a mobile-specific shell rather than trying to squeeze the desktop sidebar.

1. Main viewport first.
   - The globe/map/torus should take the full screen by default.
   - UI controls should appear as bottom sheets or compact toolbars.

2. Replace hover-only interactions.
   - Satellite and ISL info should open on tap.
   - Long-press can open context actions.
   - Tooltips should have tap equivalents.

3. Separate presentation from editing.
   - Presentation mode should have minimal controls: previous, next, pause, spin, zoom.
   - Editing mode can use panels and sheets.

4. Use responsive control groups.
   - Put common controls in a small bottom toolbar.
   - Move advanced settings into collapsible sheets.
   - Use larger hit targets for buttons, checkboxes, and sliders.

5. Make floating windows viewport-aware.
   - Clamp window sizes to the visible screen.
   - Prefer modal sheets on narrow screens.
   - Avoid spawning multiple overlapping windows on phones.

## Implementation Steps

1. Add device/layout classification.
   - Derive `LayoutMode::Desktop | Tablet | Phone` from available width, height, and pointer/touch capabilities.
   - Keep this in viewer/app state so UI code can branch cleanly.

2. Add mobile presentation controls.
   - Bottom overlay with previous/next, pause/resume, auto-rotate, and zoom.
   - Hide the desktop side-panel handle by default on phone layout.

3. Convert info hover to tap.
   - Reuse existing hover hit-testing for satellites and links.
   - Store the selected object and show an info sheet.

4. Add mobile settings sheets.
   - Start with constellation visibility, view mode, texture resolution, and simulation speed.
   - Defer advanced constellation editing until the basic demo flow is good.

5. Audit all windows.
   - Constrain size and position.
   - Replace the most important windows with bottom sheets in phone mode.

## Testing

- Test with Safari and Chrome on iPhone-sized and iPad-sized viewports.
- Verify touch rotation, pinch zoom, slide navigation, and tap selection.
- Check both `/demo` and `/presentation`.
- Check desktop remains unchanged.

## Non-Goals For First Pass

- Full constellation editing on phones.
- Replacing all desktop settings.
- Native mobile app packaging.

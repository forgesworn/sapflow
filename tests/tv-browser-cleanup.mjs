// Test fixture cleanup only; playback assertions run before this boundary.
export async function closeRoutedBrowser(browser, context) {
  try {
    // Pending file reads or loopback fetches may finish after their route has
    // been cancelled. Suppress only callbacks still running after unrouting;
    // errors during the playback assertions remain fatal.
    await context?.unrouteAll({behavior: 'ignoreErrors'});
  } finally {
    await browser.close();
  }
}

/*
 * Second-opinion ScreenCaptureKit probe (PDOOM-1298).
 *
 * WHY: the recorder's output can be black for three very different reasons, and the right
 * response differs for each:
 *   1. OBS's capture stream is wedged (bound mid-transition) while the app is perfectly
 *      capturable -> a recorder restart fixes it.
 *   2. The screen genuinely shows black (an empty pure-black terminal) -> restarting is
 *      wrong; nothing is broken.
 *   3. ScreenCaptureKit itself is wedged OS-wide (the "reboot your Mac" class) -> a
 *      recorder restart won't help, but trying once and then telling the user is right.
 *
 * The recording pixels alone cannot tell these apart. This probe can: it opens OUR OWN
 * short-lived SCK stream for the same target and reports what a FRESH binding sees.
 *   - fresh stream shows content, recording is black  -> case 1, restart.
 *   - fresh stream also shows only black              -> case 2, leave it alone.
 *   - fresh stream delivers no frames at all          -> case 3 (matches the OS-wedge
 *     forensics where a bare SCK stream produced zero frames).
 *
 * Runs synchronously with a hard deadline, on the caller's (engine) thread — never the main
 * thread. Everything is best-effort: any failure returns UNAVAILABLE and the caller falls
 * back to behaving as if the probe didn't exist (fail open).
 */

#import <Foundation/Foundation.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#import <CoreMedia/CoreMedia.h>
#import <CoreVideo/CoreVideo.h>
#include <stdatomic.h>

// Verdicts, mirrored by the Rust caller (sck_probe_verdict in engine.rs).
enum {
    SCK_PROBE_UNAVAILABLE = 0, // error / timeout / API missing -> caller must fail open
    SCK_PROBE_NO_FRAMES = 1,   // stream started but delivered nothing (OS-wedge signature)
    SCK_PROBE_BLACK = 2,       // frames delivered, essentially all-black content
    SCK_PROBE_CONTENT = 3,     // frames delivered with real content
};

// Same calibration as the Rust-side probe: limited-range capture-black is Y=16-17, the
// darkest real UI backgrounds are Y>=30, and >=97% black pixels counts as "black frame".
static const uint8_t kBlackYMax = 20;
static const double kBlackFraction = 0.97;

@interface CCProbeOutput : NSObject <SCStreamOutput>
@property(atomic) int sawContent;   // any frame below the black fraction
@property(atomic) int sawBlack;     // any frame at/above the black fraction
@end

@implementation CCProbeOutput
- (void)stream:(SCStream *)stream
    didOutputSampleBuffer:(CMSampleBufferRef)sampleBuffer
                   ofType:(SCStreamOutputType)type {
    if (type != SCStreamOutputTypeScreen) {
        return;
    }
    CVImageBufferRef img = CMSampleBufferGetImageBuffer(sampleBuffer);
    if (img == NULL) {
        return; // status-only sample (idle/blank), no pixels to judge
    }
    if (CVPixelBufferLockBaseAddress(img, kCVPixelBufferLock_ReadOnly) != kCVReturnSuccess) {
        return;
    }
    // We request 420v (bi-planar YCbCr); plane 0 is luma. Fall back to plane-less layouts
    // by just not judging the frame rather than guessing at strides.
    size_t width = 0, height = 0, stride = 0;
    const uint8_t *base = NULL;
    if (CVPixelBufferIsPlanar(img) && CVPixelBufferGetPlaneCount(img) >= 1) {
        width = CVPixelBufferGetWidthOfPlane(img, 0);
        height = CVPixelBufferGetHeightOfPlane(img, 0);
        stride = CVPixelBufferGetBytesPerRowOfPlane(img, 0);
        base = CVPixelBufferGetBaseAddressOfPlane(img, 0);
    }
    if (base != NULL && width > 0 && height > 0 && stride >= width) {
        size_t black = 0, total = 0;
        for (size_t row = 0; row < height; row++) {
            const uint8_t *line = base + row * stride;
            for (size_t col = 0; col < width; col++) {
                if (line[col] <= kBlackYMax) {
                    black++;
                }
            }
            total += width;
        }
        if (total > 0) {
            double fraction = (double)black / (double)total;
            if (fraction >= kBlackFraction) {
                self.sawBlack = 1;
            } else {
                self.sawContent = 1;
            }
        }
    }
    CVPixelBufferUnlockBaseAddress(img, kCVPixelBufferLock_ReadOnly);
}
@end

/*
 * Open a fresh SCK stream for `bundle_id` (or the main display when NULL / "__display__")
 * for up to `budget_secs`, and report what it saw. Blocks the calling thread for at most
 * roughly the budget; must not be called on the main thread.
 */
int sck_probe_capture(const char *bundle_id, double budget_secs) {
    if (budget_secs <= 0.0 || budget_secs > 10.0) {
        budget_secs = 2.5;
    }
    if (NSThread.isMainThread) {
        return SCK_PROBE_UNAVAILABLE; // never risk deadlocking the UI run loop
    }

    NSString *wantedBundle = nil;
    if (bundle_id != NULL) {
        NSString *b = [NSString stringWithUTF8String:bundle_id];
        if (b.length > 0 && ![b isEqualToString:@"__display__"]) {
            wantedBundle = b;
        }
    }

    // 1. Enumerate shareable content (async API; bounded wait).
    __block SCShareableContent *content = nil;
    dispatch_semaphore_t contentSem = dispatch_semaphore_create(0);
    [SCShareableContent getShareableContentWithCompletionHandler:^(
                            SCShareableContent *c, NSError *error) {
        if (error == nil) {
            content = c;
        }
        dispatch_semaphore_signal(contentSem);
    }];
    if (dispatch_semaphore_wait(
            contentSem,
            dispatch_time(DISPATCH_TIME_NOW, (int64_t)(budget_secs * 0.4 * NSEC_PER_SEC))) != 0 ||
        content == nil || content.displays.count == 0) {
        return SCK_PROBE_UNAVAILABLE;
    }

    SCDisplay *display = content.displays.firstObject;
    SCContentFilter *filter = nil;
    if (wantedBundle != nil) {
        SCRunningApplication *target = nil;
        for (SCRunningApplication *app in content.applications) {
            if ([app.bundleIdentifier isEqualToString:wantedBundle]) {
                target = app;
                break;
            }
        }
        if (target == nil) {
            // The app the recording is keyed to isn't running per SCK — a fresh bind can't
            // see it either, so there is nothing meaningful to compare. Fail open.
            return SCK_PROBE_UNAVAILABLE;
        }
        filter = [[SCContentFilter alloc] initWithDisplay:display
                                    includingApplications:@[ target ]
                                         exceptingWindows:@[]];
    } else {
        filter = [[SCContentFilter alloc] initWithDisplay:display excludingWindows:@[]];
    }

    SCStreamConfiguration *config = [[SCStreamConfiguration alloc] init];
    // Tiny and fast: this is a yes/no question, not a recording.
    config.width = 320;
    config.height = 180;
    config.minimumFrameInterval = CMTimeMake(1, 10); // up to 10 fps
    config.pixelFormat = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange;
    config.showsCursor = NO;
    config.queueDepth = 3;

    CCProbeOutput *output = [[CCProbeOutput alloc] init];
    SCStream *stream = [[SCStream alloc] initWithFilter:filter configuration:config delegate:nil];
    if (stream == nil) {
        return SCK_PROBE_UNAVAILABLE;
    }

    NSError *addErr = nil;
    dispatch_queue_t queue =
        dispatch_queue_create("dev.crowd-cast.sck-probe", DISPATCH_QUEUE_SERIAL);
    if (![stream addStreamOutput:output
                            type:SCStreamOutputTypeScreen
              sampleHandlerQueue:queue
                           error:&addErr]) {
        return SCK_PROBE_UNAVAILABLE;
    }

    __block BOOL started = NO;
    dispatch_semaphore_t startSem = dispatch_semaphore_create(0);
    [stream startCaptureWithCompletionHandler:^(NSError *error) {
        started = (error == nil);
        dispatch_semaphore_signal(startSem);
    }];
    if (dispatch_semaphore_wait(
            startSem,
            dispatch_time(DISPATCH_TIME_NOW, (int64_t)(budget_secs * 0.4 * NSEC_PER_SEC))) != 0 ||
        !started) {
        // Couldn't start a fresh stream inside the budget. That's itself wedge-like, but we
        // can't distinguish it from transient slowness — report it as "no frames" only when
        // the start SUCCEEDED and nothing arrived; here, fail open.
        [stream stopCaptureWithCompletionHandler:^(NSError *e){ (void)e; }];
        return SCK_PROBE_UNAVAILABLE;
    }

    // 2. Give the stream the rest of the budget to deliver frames. Stop early on a content
    //    verdict; black needs a couple of consistent frames to be believable.
    NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:budget_secs * 0.6];
    while ([deadline timeIntervalSinceNow] > 0) {
        if (output.sawContent) {
            break;
        }
        usleep(100 * 1000); // 100ms; bounded by the budget
    }

    dispatch_semaphore_t stopSem = dispatch_semaphore_create(0);
    [stream stopCaptureWithCompletionHandler:^(NSError *e) {
        (void)e;
        dispatch_semaphore_signal(stopSem);
    }];
    // Best-effort stop; don't burn more than a moment waiting for it.
    dispatch_semaphore_wait(stopSem, dispatch_time(DISPATCH_TIME_NOW, 1 * NSEC_PER_SEC));

    if (output.sawContent) {
        return SCK_PROBE_CONTENT;
    }
    if (output.sawBlack) {
        return SCK_PROBE_BLACK;
    }
    return SCK_PROBE_NO_FRAMES;
}

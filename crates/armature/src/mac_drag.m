// Starts a system drag of files from a window, and reports where the
// pointer is while another app's drag is over it. Called from platform.rs.

#import <AppKit/AppKit.h>

@interface ArmatureDragSource : NSObject <NSDraggingSource>
@end

@implementation ArmatureDragSource
- (NSDragOperation)draggingSession:(NSDraggingSession *)session sourceOperationMaskForDraggingContext:(NSDraggingContext)context {
    // The app the files land in decides whether to copy or move them.
    return NSDragOperationCopy | NSDragOperationMove | NSDragOperationLink | NSDragOperationGeneric;
}
@end

// Begins dragging the files at `paths` from `ns_view`. Must be called while
// the mouse event that started the drag is being handled. Returns 1 if a
// drag began.
int armature_drag_files(void *ns_view, const char *const *paths, int count) {
    @autoreleasepool {
        NSView *view = (__bridge NSView *)ns_view;
        NSEvent *event = NSApp.currentEvent;
        if (view == nil || event == nil || count <= 0) {
            return 0;
        }
        if (event.type != NSEventTypeLeftMouseDown && event.type != NSEventTypeLeftMouseDragged) {
            return 0;
        }
        NSPoint at = [view convertPoint:event.locationInWindow fromView:nil];
        NSMutableArray<NSDraggingItem *> *items = [NSMutableArray array];
        for (int i = 0; i < count; i++) {
            NSString *path = @(paths[i]);
            NSDraggingItem *item = [[NSDraggingItem alloc] initWithPasteboardWriter:[NSURL fileURLWithPath:path]];
            NSImage *icon = [[NSWorkspace sharedWorkspace] iconForFile:path];
            // A small fan of icons, the first under the pointer.
            CGFloat shift = MIN(i, 4) * 6.0;
            [item setDraggingFrame:NSMakeRect(at.x - 20 + shift, at.y - 20 - shift, 40, 40) contents:icon];
            [items addObject:item];
        }
        static ArmatureDragSource *source;
        if (source == nil) {
            source = [ArmatureDragSource new];
        }
        NSDraggingSession *session = [view beginDraggingSessionWithItems:items event:event source:source];
        session.animatesToStartingPositionsOnCancelOrFail = YES;
        return 1;
    }
}

// The pointer's position in the view, from the top-left corner in points.
// Works during a drag from another app, when no mouse events arrive.
int armature_pointer_in_view(void *ns_view, double *x, double *y) {
    NSView *view = (__bridge NSView *)ns_view;
    if (view == nil || view.window == nil) {
        return 0;
    }
    NSPoint p = [view convertPoint:view.window.mouseLocationOutsideOfEventStream fromView:nil];
    *x = p.x;
    *y = view.isFlipped ? p.y : view.bounds.size.height - p.y;
    return 1;
}

// Tells of the app being asked to open again while it is running: its
// Dock icon clicked, or opened again from the Finder or a launcher. An app
// whose window is out of sight hears nothing else of it.
static void (*armature_reopen_callback)(void);

@interface ArmatureReopen : NSObject
@end

@implementation ArmatureReopen
- (void)handle:(NSAppleEventDescriptor *)event withReply:(NSAppleEventDescriptor *)reply {
    if (armature_reopen_callback != NULL) {
        armature_reopen_callback();
    }
}
@end

// Has `callback` called, on the main thread, each time the app is asked to
// open again. To be called once the app has finished launching: AppKit
// sets its own handler for this as it launches, which this takes over.
void armature_watch_reopen(void (*callback)(void)) {
    armature_reopen_callback = callback;
    static ArmatureReopen *handler;
    if (handler == nil) {
        handler = [ArmatureReopen new];
    }
    // The Apple event 'aevt'/'rapp': reopen application.
    [[NSAppleEventManager sharedAppleEventManager] setEventHandler:handler andSelector:@selector(handle:withReply:) forEventClass:'aevt' andEventID:'rapp'];
}

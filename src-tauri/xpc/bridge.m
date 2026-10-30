// Public XPC/Security APIs only. No paths, bookmarks, descriptors or endpoints in the protocol.
#import <Foundation/Foundation.h>
#import <Security/Security.h>
#include <xpc/xpc.h>
#include <sys/resource.h>
#include <stdatomic.h>
#include <unistd.h>
#include <mach/mach.h>

#define SERVICE "app.mori.viewer.decoder"
#define APP "app.mori.viewer"
#define TEAM "AYA5S225VU"
#define MORI_MAX_INPUT (160UL * 1024 * 1024)
#define MORI_MAX_OUTPUT (64UL * 1024 * 1024 + 16)
// The Store re-signs code with Apple Mac OS Application Signing. TN3127's
// Mac App Store branch authenticates Apple's certificate OID plus exact identity;
// local Development/Distribution builds additionally require our Team ID.
#define REQUIREMENT(ID) "anchor apple generic and identifier \"" ID "\" and (certificate leaf[subject.OU] = \"" TEAM "\" or certificate leaf[field.1.2.840.113635.100.6.1.9] exists) and entitlement[\"com.apple.security.app-sandbox\"] exists"

static bool typed(xpc_object_t m, const char *key, xpc_type_t type) {
    xpc_object_t v = xpc_dictionary_get_value(m, key);
    return v && xpc_get_type(v) == type;
}

#ifndef MORI_SERVICE
@interface MoriReply : NSObject
@property(nonatomic) dispatch_semaphore_t ready;
@property(nonatomic) NSData *bytes;
@property(nonatomic) int status;
@property(nonatomic) BOOL finished;
@end
@implementation MoriReply
@end

// Validate the sealed embedded executable before sending private bytes. Peer requirements
// authenticate incoming messages, not delivery of the first outgoing message.
static bool embedded_service_valid(void) {
    NSURL *url = [[[NSBundle mainBundle] bundleURL] URLByAppendingPathComponent:@"Contents/XPCServices/app.mori.viewer.decoder.xpc"];
    SecStaticCodeRef code = NULL; SecRequirementRef req = NULL;
    bool ok = SecRequirementCreateWithString(CFSTR(REQUIREMENT(SERVICE)),0,&req)==errSecSuccess
        && SecStaticCodeCreateWithPath((__bridge CFURLRef)url,0,&code)==errSecSuccess
        && SecStaticCodeCheckValidity(code,kSecCSStrictValidate | kSecCSCheckAllArchitectures,req)==errSecSuccess;
    if(code)CFRelease(code); if(req)CFRelease(req);
    return ok;
}

// Synchronous C ABI; the reply block owns its state even after a timeout.
int mori_xpc_call(const char *op, uint32_t edge, const uint8_t *input, size_t length,
                  uint64_t timeout_ms, uint8_t **output, size_t *output_len) {
    @autoreleasepool {
        *output = NULL; *output_len = 0;
        if (!op || strlen(op) > 16 || length > MORI_MAX_INPUT || edge < 32 || edge > 4096
            || timeout_ms == 0 || timeout_ms > 30000) return 3;
        uint64_t started = clock_gettime_nsec_np(CLOCK_MONOTONIC);
        if (!embedded_service_valid()) return 7;
        uint64_t elapsed = (clock_gettime_nsec_np(CLOCK_MONOTONIC) - started) / NSEC_PER_MSEC;
        if (elapsed >= timeout_ms) return 8;
        timeout_ms -= elapsed;
        MoriReply *state = [MoriReply new];
        state.ready = dispatch_semaphore_create(0);
        state.status = 7;
        xpc_connection_t c = xpc_connection_create(SERVICE, NULL);
        if (!c) return 7;
        // macOS 12+ validates every incoming message against the peer requirement.
        if (@available(macOS 12.0, *)) {
            if (xpc_connection_set_peer_code_signing_requirement(c, REQUIREMENT(SERVICE)) != 0) return 7;
        } else { return 7; }
        xpc_connection_set_event_handler(c, ^(xpc_object_t event) {
            if (xpc_get_type(event) == XPC_TYPE_ERROR) {
                @synchronized(state) { if (!state.finished) { state.finished = YES; dispatch_semaphore_signal(state.ready); } }
            }
        });
        xpc_connection_resume(c);
        xpc_object_t request = xpc_dictionary_create(NULL, NULL, 0);
        xpc_dictionary_set_uint64(request, "version", 1);
        xpc_dictionary_set_string(request, "op", op);
        xpc_dictionary_set_uint64(request, "edge", edge);
        xpc_dictionary_set_uint64(request, "timeout", timeout_ms);
        xpc_dictionary_set_data(request, "input", input, length);
        xpc_connection_send_message_with_reply(c, request, dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^(xpc_object_t reply) {
            @autoreleasepool {
                @synchronized(state) {
                    if (state.finished) return;
                    if (xpc_get_type(reply) == XPC_TYPE_DICTIONARY && xpc_dictionary_get_count(reply) == 3
                        && typed(reply, "version", XPC_TYPE_UINT64) && xpc_dictionary_get_uint64(reply, "version") == 1
                        && typed(reply, "status", XPC_TYPE_INT64) && typed(reply, "output", XPC_TYPE_DATA)) {
                        size_t len = 0;
                        const void *data = xpc_dictionary_get_data(reply, "output", &len);
                        int64_t status = xpc_dictionary_get_int64(reply, "status");
                        if (len <= MORI_MAX_OUTPUT && status >= 0 && status <= 7 && (status == 0 || len == 0)) {
                            state.bytes = [NSData dataWithBytes:data length:len];
                            state.status = (int)status;
                        }
                    }
                    state.finished = YES;
                    dispatch_semaphore_signal(state.ready);
                }
            }
        });
        long wait = dispatch_semaphore_wait(state.ready, dispatch_time(DISPATCH_TIME_NOW, timeout_ms * NSEC_PER_MSEC));
        int status;
        @synchronized(state) {
            state.finished = YES;
            status = wait ? 8 : state.status;
            if (status == 0) {
                *output_len = state.bytes.length;
                *output = malloc(*output_len ? *output_len : 1);
                if (!*output) { *output_len = 0; status = 3; }
                else if (*output_len) memcpy(*output, state.bytes.bytes, *output_len);
            }
        }
        xpc_connection_cancel(c);
        return status;
    }
}
#else
#include <errno.h>
#ifdef MORI_XPC_TESTING
#include <pwd.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <fcntl.h>
#include <spawn.h>
#include <sys/wait.h>
#endif

typedef int (*decode_fn)(const char *, uint32_t, const uint8_t *, size_t, uint8_t **, size_t *);
static decode_fn decoder;
static atomic_uint_fast64_t generation;
static atomic_uint_fast64_t active;

static void reply_with(xpc_object_t request, int status, const void *data, size_t length) {
    xpc_object_t reply = xpc_dictionary_create_reply(request);
    if (!reply) return;
    xpc_dictionary_set_uint64(reply, "version", 1);
    xpc_dictionary_set_int64(reply, "status", status);
    xpc_dictionary_set_data(reply, "output", data, length);
    xpc_connection_send_message(xpc_dictionary_get_remote_connection(request), reply);
}
static bool self_sandboxed(void) {
    SecCodeRef code = NULL;
    CFDictionaryRef info = NULL;
    bool ok = false;
    if (SecCodeCopySelf(0, &code) == errSecSuccess && SecCodeCopySigningInformation(code, kSecCSSigningInformation, &info) == errSecSuccess) {
        CFDictionaryRef ent = CFDictionaryGetValue(info, kSecCodeInfoEntitlementsDict);
        ok = ent && CFDictionaryGetValue(ent, CFSTR("com.apple.security.app-sandbox")) == kCFBooleanTrue;
        if (ent) {
            NSString *team = ((__bridge NSDictionary *)ent)[@"com.apple.developer.team-identifier"];
            NSString *identity = ((__bridge NSDictionary *)ent)[@"com.apple.application-identifier"];
            if (team && ![team isEqualToString:@TEAM]) ok = false;
            if (identity && ![identity isEqualToString:@TEAM @"." @SERVICE]) ok = false;
            NSSet *allowed = [NSSet setWithArray:@[@"com.apple.security.app-sandbox", @"com.apple.application-identifier", @"com.apple.developer.team-identifier"]];
            for (NSString *key in (__bridge NSDictionary *)ent) if (![allowed containsObject:key]) ok = false;
        }
        const CFStringRef forbidden[] = {CFSTR("com.apple.security.inherit"), CFSTR("com.apple.security.network.client"),
            CFSTR("com.apple.security.network.server"), CFSTR("com.apple.security.files.user-selected.read-write"),
            CFSTR("com.apple.security.files.user-selected.read-only"), CFSTR("com.apple.security.files.bookmarks.app-scope"),
            CFSTR("com.apple.security.application-groups")};
        for (size_t i=0; i<sizeof(forbidden)/sizeof(forbidden[0]); ++i) if (ent && CFDictionaryContainsKey(ent, forbidden[i])) ok = false;
    }
    if (info) CFRelease(info);
    if (code) CFRelease(code);
    return ok;
}
static void request_received(xpc_object_t request, xpc_connection_t peer) {
    // The connection enforces the app identifier, Apple anchor, team and sandbox entitlement.
    (void)peer;
    if (xpc_dictionary_get_count(request) != 5 || !typed(request,"version",XPC_TYPE_UINT64) || xpc_dictionary_get_uint64(request,"version") != 1
        || !typed(request,"op",XPC_TYPE_STRING) || !typed(request,"edge",XPC_TYPE_UINT64)
        || !typed(request,"timeout",XPC_TYPE_UINT64) || !typed(request,"input",XPC_TYPE_DATA)) {
        reply_with(request,6,NULL,0); return;
    }
    size_t length = 0;
    const uint8_t *input = xpc_dictionary_get_data(request,"input",&length);
    const char *op = xpc_dictionary_get_string(request,"op");
    uint64_t edge = xpc_dictionary_get_uint64(request,"edge");
    uint64_t timeout = xpc_dictionary_get_uint64(request,"timeout");
    if (length > MORI_MAX_INPUT || strlen(op)>16 || edge<32 || edge>4096 || timeout==0 || timeout>30000) {
        reply_with(request,3,NULL,0); return;
    }
    uint64_t job = atomic_fetch_add(&generation,1)+1;
    uint64_t expected = 0;
    if (!atomic_compare_exchange_strong(&active,&expected,job)) { reply_with(request,7,NULL,0); return; }
    // Timer runs independently of the decoder queue, even after the client times out.
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW,timeout*NSEC_PER_MSEC), dispatch_get_global_queue(QOS_CLASS_USER_INTERACTIVE,0), ^{
        if (atomic_load(&active)==job) _exit(8);
    });
    // Native ImageIO/CoreGraphics allocations are outside Rust's allocation budget.
    // Observe physical footprint on a separate queue and abort the whole service.
    // This is a watchdog (25ms sampling), not a promise of zero transient overshoot.
    uint64_t memory_budget = 1024ULL * 1024 * 1024;
#ifdef MORI_XPC_TESTING
    if (!strcmp(op,"_memory")) memory_budget = 64ULL * 1024 * 1024;
#endif
    dispatch_source_t memory = dispatch_source_create(DISPATCH_SOURCE_TYPE_TIMER,0,0,dispatch_get_global_queue(QOS_CLASS_USER_INTERACTIVE,0));
    dispatch_source_set_timer(memory,DISPATCH_TIME_NOW,25*NSEC_PER_MSEC,1*NSEC_PER_MSEC);
    dispatch_source_set_event_handler(memory,^{
        if (atomic_load(&active)!=job) return;
        task_vm_info_data_t vm; mach_msg_type_number_t count=TASK_VM_INFO_COUNT;
        if (task_info(mach_task_self(),TASK_VM_INFO,(task_info_t)&vm,&count)!=KERN_SUCCESS || vm.phys_footprint>memory_budget) _exit(3);
    });
    dispatch_resume(memory);
    dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED,0), ^{
        @autoreleasepool {
            uint8_t *output = NULL; size_t out_len = 0;
            int status;
#ifdef MORI_XPC_TESTING
            if (!strcmp(op,"_hang")) { for (;;) pause(); }
            if (!strcmp(op,"_memory")) {
                volatile uint8_t *pressure=malloc(128UL*1024*1024);
                if (!pressure) _exit(3);
                for(size_t i=0;i<128UL*1024*1024;i+=4096) pressure[i]=1;
                for (;;) pause();
            }
            if (!strcmp(op,"_security")) {
                struct passwd *pw = getpwuid(getuid());
                NSString *path = [NSString stringWithFormat:@"%s/Library/Containers/app.mori.viewer/Data/Library/Caches/mori-xpc-denial-fixture",pw->pw_dir];
                errno=0; int fd=open(path.fileSystemRepresentation,O_RDONLY); bool rd=fd<0 && errno==EPERM; if(fd>=0)close(fd);
                errno=0; fd=open(path.fileSystemRepresentation,O_WRONLY); bool wr=fd<0 && errno==EPERM; if(fd>=0)close(fd);
                int sock=socket(AF_INET,SOCK_STREAM,0); bool net=sock<0 && errno==EPERM;
                if(sock>=0) { struct sockaddr_in a={.sin_len=sizeof(a),.sin_family=AF_INET,.sin_addr.s_addr=htonl(INADDR_LOOPBACK)}; errno=0; net=bind(sock,(struct sockaddr*)&a,sizeof(a))<0 && errno==EPERM; close(sock); }
                sock=socket(AF_INET,SOCK_STREAM,0);
                bool outbound=sock<0 && errno==EPERM;
                if(sock>=0) { struct sockaddr_in a={.sin_len=sizeof(a),.sin_family=AF_INET,.sin_port=htons(9),.sin_addr.s_addr=htonl(INADDR_LOOPBACK)}; errno=0; outbound=connect(sock,(struct sockaddr*)&a,sizeof(a))<0 && errno==EPERM; close(sock); }
                char *args[]={"/usr/bin/true",NULL}; char *env[]={NULL}; pid_t pid=0;
                int spawn=posix_spawn(&pid,args[0],NULL,NULL,args,env); if(spawn==0)waitpid(pid,NULL,0);
                bool own=[NSHomeDirectory() containsString:@"/app.mori.viewer.decoder/Data"];
                NSDictionary *report=@{@"user_read_denied":@(rd),@"user_write_denied":@(wr),@"network_denied":@(net&&outbound),@"spawn_denied":@(spawn!=0),@"own_container":@(own)};
                NSData *json=[NSJSONSerialization dataWithJSONObject:report options:0 error:NULL];
                out_len=16+json.length; output=calloc(1,out_len); memcpy(output,"MORI",4); output[12]=7; memcpy(output+16,json.bytes,json.length); status=0;
            } else
#endif
            status=decoder(op,(uint32_t)edge,input,length,&output,&out_len);
            if (out_len>MORI_MAX_OUTPUT || (status==0 && out_len<16)) { status=3; out_len=0; }
            if (status!=0) out_len=0;
            dispatch_source_cancel(memory);
            atomic_store(&active,0);
            reply_with(request,status,output,out_len);
            free(output);
        }
    });
}
static void accept_peer(xpc_connection_t peer) {
            if (@available(macOS 12.0,*)) {
                if (xpc_connection_set_peer_code_signing_requirement(peer,REQUIREMENT(APP))!=0) _exit(5);
            } else { _exit(5); }
            xpc_connection_set_event_handler(peer,^(xpc_object_t event) {
                if (xpc_get_type(event)==XPC_TYPE_DICTIONARY) request_received(event,peer);

            });
            xpc_connection_resume(peer);
}
void mori_service_main(decode_fn function) {
    @autoreleasepool {
        if (![[NSBundle mainBundle].bundleIdentifier isEqualToString:@SERVICE] || !self_sandboxed()) _exit(5);
        // Keep XPC/framework descriptors available. Memory bounds are enforced by the decoders.
        struct rlimit zero={0,0}, files={128,128}, cpu={60,60};
        if (setrlimit(RLIMIT_CPU,&cpu) || setrlimit(RLIMIT_CORE,&zero) || setrlimit(RLIMIT_NPROC,&zero) || setrlimit(RLIMIT_FSIZE,&zero) || setrlimit(RLIMIT_NOFILE,&files)) _exit(5);
        signal(SIGXFSZ,SIG_IGN);
        decoder=function;
        xpc_main(accept_peer);
    }
}
#endif

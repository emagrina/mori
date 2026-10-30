// Test client only. Built into a temporary signed app by smoke-test.py.
#import <Foundation/Foundation.h>
#include <stdio.h>
#include <xpc/xpc.h>
extern int mori_xpc_call(const char *,uint32_t,const uint8_t *,size_t,uint64_t,uint8_t **,size_t *);
static NSData *fixture(const char *name) {
    return [NSData dataWithContentsOfFile:[[[NSBundle mainBundle] resourcePath] stringByAppendingPathComponent:@(name)]];
}
static int call(const char *op,NSData *data,uint64_t timeout,uint32_t edge,BOOL emit) {
    uint8_t *out=NULL; size_t len=0;
    int status=mori_xpc_call(op,edge,data.bytes,data.length,timeout,&out,&len);
    fprintf(stderr,"op=%s status=%d bytes=%zu\n",op,status,len);
    if(emit && status==0 && len)fwrite(out,1,len,stdout);
    free(out); return status;
}
int main(int argc,const char **argv) {
    @autoreleasepool {
        if(argc<3)return 99;
        NSData *data=fixture(argv[2]); if(!data)return 98;
        if(!strcmp(argv[1],"_restart") || !strcmp(argv[1],"_memoryrestart")) {
            if(call("thumb",fixture("tiny.png"),15000,128,NO)!=0)return 93;
            NSTimeInterval started=NSProcessInfo.processInfo.systemUptime;
            int first=call(!strcmp(argv[1],"_restart")?"_hang":"_memory",data,!strcmp(argv[1],"_restart")?500:5000,128,NO);
            if(first!=8 && first!=7)return 97;
            NSTimeInterval elapsed=NSProcessInfo.processInfo.systemUptime-started;
            if(!strcmp(argv[1],"_memoryrestart") && elapsed>2.0)return 92;
            if(!strcmp(argv[1],"_restart") && (elapsed<0.35 || elapsed>2.0))return 91;
            // launchd owns restart; stay in the SAME host process/bootstrap namespace.
            for(int i=0;i<15;i++) {
                usleep(200000);
                if(call("thumb",fixture("tiny.png"),3000,128,YES)==0)return 0;
            }
            return 96;
        }
        if(!strcmp(argv[1],"_security")) {
            NSString *cache=NSSearchPathForDirectoriesInDomains(NSCachesDirectory,NSUserDomainMask,YES).firstObject;
            [[NSFileManager defaultManager] createDirectoryAtPath:cache withIntermediateDirectories:YES attributes:nil error:NULL];
            NSString *sentinel=[cache stringByAppendingPathComponent:@"mori-xpc-denial-fixture"];
            if(![@"synthetic fixture" writeToFile:sentinel atomically:YES encoding:NSUTF8StringEncoding error:NULL])return 95;
            int result=call(argv[1],data,15000,128,YES);
            [[NSFileManager defaultManager] removeItemAtPath:sentinel error:NULL]; return result;
        }
        if(!strcmp(argv[1],"_badversion")) {
            dispatch_semaphore_t done=dispatch_semaphore_create(0); __block int status=94;
            xpc_connection_t c=xpc_connection_create("app.mori.viewer.decoder",NULL);
            xpc_connection_set_event_handler(c,^(xpc_object_t event){(void)event;}); xpc_connection_resume(c);
            xpc_object_t req=xpc_dictionary_create(NULL,NULL,0);
            xpc_dictionary_set_uint64(req,"version",2); xpc_dictionary_set_string(req,"op","thumb");
            xpc_dictionary_set_uint64(req,"edge",128); xpc_dictionary_set_uint64(req,"timeout",1000);
            xpc_dictionary_set_data(req,"input",data.bytes,data.length);
            xpc_connection_send_message_with_reply(c,req,dispatch_get_global_queue(0,0),^(xpc_object_t reply){
                if(xpc_get_type(reply)==XPC_TYPE_DICTIONARY)status=(int)xpc_dictionary_get_int64(reply,"status");
                dispatch_semaphore_signal(done);
            });
            dispatch_semaphore_wait(done,dispatch_time(DISPATCH_TIME_NOW,5*NSEC_PER_SEC));
            xpc_connection_cancel(c); return status;
        }
        return call(argv[1],data,argc>3?strtoull(argv[3],NULL,10):15000,argc>4?atoi(argv[4]):128,YES);
    }
}

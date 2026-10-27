#!/usr/bin/env python3
import json,os,sys,time
assert sys.argv[sys.argv.index('--input-format')+1]=='stream-json'
assert sys.argv[sys.argv.index('--output-format')+1]=='stream-json'
assert sys.argv[sys.argv.index('--permission-prompt-tool')+1]=='stdio'
assert '--replay-user-messages' in sys.argv
session=sys.argv[sys.argv.index('--resume' if '--resume' in sys.argv else '--session-id')+1]
with open('launch.json','w') as output: json.dump({'argv':sys.argv[1:],'account':os.environ.get('CLAUDE_CONFIG_DIR'),'pid':os.getpid()},output)
def emit(value): print(json.dumps(value),flush=True)
def control(value): emit({'type':'control_response','response':{'subtype':'success','request_id':value['request_id'],'response':{'models':[],'account':{},'commands':[{'name':'review','description':'Review changes','argumentHint':'[path]'}]}}})
def result(reason='completed',subtype='success',identity=None,errors=None,**extra):
 emit({'type':'result','subtype':subtype,'session_id':identity or session,'uuid':'result-uuid-not-user-uuid','is_error':subtype!='success','terminal_reason':reason,'result':'Hello world','errors':errors or [],**extra})
def event(value): emit({'type':'stream_event','session_id':session,'parent_tool_use_id':None,'event':value})
def tool_use(ident,command): emit({'type':'assistant','uuid':'block-'+ident,'session_id':session,'parent_tool_use_id':None,'message':{'id':'message-'+ident,'content':[{'type':'tool_use','id':ident,'name':'Bash','input':{'command':command}}]}})
def tool_call(ident,name,arguments): emit({'type':'assistant','uuid':'block-'+ident,'session_id':session,'parent_tool_use_id':None,'message':{'id':'message-'+ident,'content':[{'type':'tool_use','id':ident,'name':name,'input':arguments}]}})
def user_event(content,**extra): emit({'type':'user','session_id':session,'parent_tool_use_id':None,'message':{'role':'user','content':content},**extra})
running=None
compacting=None
for line in sys.stdin:
 value=json.loads(line)
 if value['type']=='control_request':
  if compacting and os.path.exists('compact-done'):
   # Observed from Claude 2.1.302: a compaction with nothing to do answers before it confirms
   # the message, and the result repeats the answer.
   emit({'type':'system','subtype':'status','status':None,'session_id':session})
   emit({'type':'system','subtype':'init','session_id':session,'cwd':os.getcwd()})
   emit({'type':'assistant','uuid':'local-answer','session_id':session,'parent_tool_use_id':None,'message':{'content':[{'type':'text','text':'Not enough messages to compact.'}]}})
   echo=dict(compacting);echo['isReplay']=True;echo['message']={'role':'user','content':'<command-name>/compact</command-name>'};emit(echo)
   result(result='Not enough messages to compact.');compacting=None
  if value['request']['subtype']=='rewind_conversation':
   # Observed from Claude 2.1.301: a rewind it refuses still succeeds as a control request.
   with open('rewind.json','w') as output: json.dump(value['request'],output)
   outcome={'rewound':False,'error':'stale target','reason':'stale_target'} if os.path.exists('refuse-rewind') else {'rewound':True,'prefillText':'first','precedingAssistantUuid':None}
   emit({'type':'control_response','response':{'subtype':'success','request_id':value['request_id'],'response':outcome}})
   continue
  if value['request']['subtype']=='stop_task':
   # Claude acknowledges a stop, then reports the task stopped.
   control(value)
   emit({'type':'system','subtype':'task_notification','session_id':session,'task_id':value['request']['task_id'],'status':'stopped','summary':'Stopped'})
   continue
  if value['request']['subtype']=='interrupt':
   # An idle Claude acknowledges an interrupt and reports nothing else.
   if os.path.exists('ignore-interrupt'): control(value);continue
   if running:
    # Claude 2.1.296's observed order: acknowledge, reject the running tool, end the turn.
    control(value)
    user_event([{'type':'tool_result','tool_use_id':running,'content':"The user doesn't want to proceed with this tool use.",'is_error':True}],tool_result_meta=[{'id':running,'non_execution_kind':'user-rejected'}])
    user_event([{'type':'text','text':'[Request interrupted by user]'}])
    result('aborted_streaming','error_during_execution');running=None;continue
   result('aborted_streaming')
  control(value)
 elif value['type']=='user':
  prompt=value['message']['content']
  steer=value.get('priority')=='next'
  if steer and running:
   with open('prompt.json','w') as output: json.dump(value,output)
   # Observed from Claude 2.1.300: the running tool finishes, Claude replays the steer right
   # after its result, and the same turn continues to one result.
   user_event([{'type':'tool_result','tool_use_id':running,'content':'one','is_error':False}])
   # Claude downscales large images and echoes them re-encoded as JPEG.
   echo=dict(value);echo['isReplay']=True
   if isinstance(prompt,list): echo['message']=dict(value['message'],content=[dict(block,source=dict(block['source'],media_type='image/jpeg')) if block['type']=='image' else block for block in prompt])
   emit(echo)
   running='steer-call';tool_use(running,'sleep 20');continue
  with open('prompt.json','w') as output: json.dump(value,output)
  if prompt=='oversize': sys.stdout.write('x'*(1024*1024+1)+'\n');sys.stdout.flush();continue
  if prompt=='partial': sys.stdout.write('{"type":"result"}');sys.stdout.flush();break
  if prompt=='/compact':
   # Observed from Claude 2.1.302: `/compact` as a resumed process's first input reports
   # `compacting` at once, and `init` only once compaction is done.
   emit({'type':'system','subtype':'status','status':'compacting','session_id':session});compacting=value;continue
  emit({'type':'system','subtype':'init','session_id':'foreign' if prompt=='wrong-session' else session,'cwd':'/foreign' if prompt=='wrong-cwd' else os.getcwd()})
  result(identity='foreign')
  result() # No user ACK yet: never a completed turn.
  if prompt=='no-ack': continue
  if prompt=='slow-ack':
   # The test lets Claude confirm this message only after it sends another.
   while not os.path.exists('ack'): time.sleep(0.01)
  echo=dict(value);echo['isReplay']=True;echo['message']={'role':'user','content':[{'type':'text','text':prompt}]};emit(echo)
  if prompt in ['permission-rules','permission-mode']:
   # Observed from Claude 2.1.299: Bash suggests a reusable rule; Write suggests accepting edits.
   suggestion={'type':'addRules','rules':[{'toolName':'Bash','ruleContent':'npm view *'}],'behavior':'allow','destination':'localSettings'} if prompt=='permission-rules' else {'type':'setMode','mode':'acceptEdits','destination':'session'}
   emit({'type':'control_request','request_id':'permission-id','request':{'subtype':'can_use_tool','tool_name':'Bash','tool_use_id':'tool-id','input':{'command':'npm view left-pad version'},'permission_suggestions':[suggestion]}})
   continue
  if prompt in ['permission','cancel','question']:
   emit({'type':'control_request','request_id':'permission-id','request':{'subtype':'can_use_tool','tool_name':'Bash','tool_use_id':'tool-id','input':{'command':'echo hello'}}})
   if prompt=='question':
    emit({'type':'control_cancel_request','request_id':'permission-id'})
    emit({'type':'control_request','request_id':'question-id','request':{'subtype':'can_use_tool','tool_name':'AskUserQuestion','tool_use_id':'question-tool','input':{'questions':[{'question':'Choose color','options':[{'label':'Red'},{'label':'Blue'}],'multiSelect':True}],'marker':'preserve'}}})
   if prompt=='cancel': emit({'type':'control_cancel_request','request_id':'permission-id'})
   continue
  if prompt=='hold': continue
  if prompt=='fail-later':
   # The test fails this turn once it has queued messages behind it.
   while not os.path.exists('fail'): time.sleep(0.01)
   result('model_error','error_during_execution');continue
  if prompt.startswith('write-'):
   emit({'type':'control_request','request_id':'write-id','request':{'subtype':'can_use_tool','tool_name':'Write','tool_use_id':'write-tool','input':{'file_path':'/tmp/large','content':'x'*int(prompt[6:])}}})
   continue
  if prompt=='flood':
   for i in range(300):
    emit({'type':'assistant','uuid':'think-'+str(i),'session_id':session,'parent_tool_use_id':None,'message':{'id':'think-message-'+str(i),'content':[{'type':'thinking','thinking':''}]}})
    emit({'type':'assistant','uuid':'flood-'+str(i),'session_id':session,'parent_tool_use_id':None,'message':{'id':'flood-message-'+str(i),'content':[{'type':'tool_use','id':'flood-call-'+str(i),'name':'Bash','input':{'command':'true'}}]}})
    emit({'type':'user','session_id':session,'parent_tool_use_id':None,'message':{'role':'user','content':[{'type':'tool_result','tool_use_id':'flood-call-'+str(i),'content':[{'type':'text','text':'ok'}],'is_error':False}]}})
  if prompt=='edit':
   # Observed from Claude 2.1.299: an Edit result carries Claude's own patch.
   emit({'type':'assistant','uuid':'block-edit','session_id':session,'parent_tool_use_id':None,'message':{'id':'message-edit','content':[{'type':'tool_use','id':'edit-call','name':'Edit','input':{'file_path':'/tmp/project/notes.txt','old_string':'line two','new_string':'line 2'}}]}})
   user_event([{'type':'tool_result','tool_use_id':'edit-call','content':'Edited'}],tool_use_result={'filePath':'/tmp/project/notes.txt','oldString':'line two','newString':'line 2','structuredPatch':[{'oldStart':1,'oldLines':3,'newStart':1,'newLines':3,'lines':[' line one','-line two','+line 2',' line three']}]})
   result();continue
  if prompt in ['tool-hold','slow-ack']:
   running='bash-call';tool_use(running,'sleep 60');continue
  if prompt=='tool-stream':
   # Observed from Claude 2.1.299: arguments arrive as partial JSON after an empty input.
   event({'type':'message_start','message':{'id':'stream-tool-message'}})
   event({'type':'content_block_start','index':0,'content_block':{'type':'tool_use','id':'stream-call','name':'Read','input':{}}})
   for part in ['','{"file_path": "/private/t','mp/notes']:
    event({'type':'content_block_delta','index':0,'delta':{'type':'input_json_delta','partial_json':part}})
   continue
  event({'type':'message_start','message':{'id':'assistant-id'}})
  event({'type':'content_block_start','index':0,'content_block':{'type':'text','text':''}})
  event({'type':'content_block_delta','index':0,'delta':{'type':'text_delta','text':'Hello '}})
  event({'type':'content_block_delta','index':0,'delta':{'type':'text_delta','text':'world'}})
  emit({'type':'assistant','uuid':'block-id','session_id':session,'parent_tool_use_id':None,'message':{'id':'assistant-id','content':[{'type':'text','text':'Hello world'}]}})
  event({'type':'content_block_stop','index':0})
  if prompt=='usage':
   # Observed from Claude 2.1.299: each model call reports its usage; the result reports the window.
   emit({'type':'assistant','uuid':'usage-block','session_id':session,'parent_tool_use_id':None,'message':{'id':'usage-message','content':[{'type':'text','text':'Counted'}],'usage':{'input_tokens':2,'cache_creation_input_tokens':900,'cache_read_input_tokens':24000,'output_tokens':40}}})
   result(modelUsage={'claude-haiku-5-5':{'contextWindow':200000},'claude-opus-5-5':{'contextWindow':1000000}},usage={'input_tokens':5,'cache_creation_input_tokens':900,'cache_read_input_tokens':48000,'output_tokens':95},total_cost_usd=0.0421)
   continue
  if prompt=='read-image':
   # Observed from Claude 2.1.299: a Read of an image repeats its base64 in tool_use_result.
   import base64,struct,zlib
   def chunk(kind,body): return struct.pack('>I',len(body))+kind+body+struct.pack('>I',zlib.crc32(kind+body))
   png=b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',struct.pack('>IIBBBBB',1,1,8,6,0,0,0))+chunk(b'tEXt',b'pad\0'+b'x'*500000)+chunk(b'IDAT',zlib.compress(b'\0\1\2\3\377'))+chunk(b'IEND',b'')
   data=base64.b64encode(png).decode()
   emit({'type':'assistant','uuid':'read-block','session_id':session,'parent_tool_use_id':None,'message':{'id':'read-message','content':[{'type':'tool_use','id':'read-call','name':'Read','input':{'file_path':'shot.png'}}]}})
   emit({'type':'user','session_id':session,'parent_tool_use_id':None,'message':{'role':'user','content':[{'type':'tool_result','tool_use_id':'read-call','content':[{'type':'image','source':{'type':'base64','data':data,'media_type':'image/png'}}]}]},'tool_use_result':{'type':'image','file':{'base64':data,'type':'image/png'}}})
  if prompt=='big-output':
   emit({'type':'assistant','uuid':'big-block','session_id':session,'parent_tool_use_id':None,'message':{'id':'big-message','content':[{'type':'tool_use','id':'read-call','name':'Bash','input':{'command':'cat big.log'}}]}})
   emit({'type':'user','session_id':session,'parent_tool_use_id':None,'message':{'role':'user','content':[{'type':'tool_result','tool_use_id':'read-call','content':[{'type':'text','text':'z'*(2*1024*1024)}]}]}})
  if prompt=='huge-line':
   emit({'type':'assistant','uuid':'huge-block','session_id':session,'parent_tool_use_id':None,'message':{'id':'huge-message','content':[{'type':'text','text':'y'*(17*1024*1024)}]}})
  if prompt in ['tool-success','tool-failure']:
   emit({'type':'assistant','uuid':'tool-block','session_id':session,'parent_tool_use_id':None,'message':{'id':'tool-message','content':[{'type':'tool_use','id':'bash-call','name':'Bash','input':{'command':'echo hello'}}]}})
   emit({'type':'user','session_id':session,'parent_tool_use_id':None,'message':{'role':'user','content':[{'type':'tool_result','tool_use_id':'bash-call','content':[{'type':'text','text':'observed output'}],'is_error':prompt=='tool-failure'}]}})
  if prompt=='limits':
   emit({'type':'rate_limit_event','rate_limit_info':{'status':'allowed_warning','rateLimitType':'five_hour','resetsAt':1760000000,'utilization':0.91},'uuid':'limit-1','session_id':session})
   emit({'type':'rate_limit_event','rate_limit_info':{'status':'rejected','rateLimitType':'seven_day_opus','resetsAt':1760500000},'uuid':'limit-2','session_id':session})
  if prompt=='compact':
   # Observed from Claude: `compacting` repeats while it works, then a boundary reports the sizes.
   emit({'type':'system','subtype':'status','status':'compacting','session_id':session})
   emit({'type':'system','subtype':'status','status':'compacting','session_id':session})
   emit({'type':'system','subtype':'compact_boundary','compact_metadata':{'trigger':'manual','pre_tokens':180000,'post_tokens':12000},'uuid':'boundary','session_id':session})
  if prompt=='todo':
   tool_call('todo-call','TodoWrite',{'todos':[{'content':'Read the code','status':'completed','activeForm':'Reading'},{'content':'Fix the bug','status':'in_progress','activeForm':'Fixing'},{'content':'Ship it','status':'pending','activeForm':'Shipping'}]})
   user_event([{'type':'tool_result','tool_use_id':'todo-call','content':'ok'}])
  if prompt=='tasks':
   for number,subject in [('1','Read the code'),('2','Fix the bug'),('3','Drop this')]:
    tool_call('task-'+number,'TaskCreate',{'subject':subject})
    user_event([{'type':'tool_result','tool_use_id':'task-'+number,'content':'created'}],tool_use_result={'task':{'id':number,'subject':subject}})
   for number,change in [('1',{'status':'completed'}),('2',{'status':'in_progress','subject':'Fix the real bug'}),('3',{'status':'deleted'})]:
    tool_call('update-'+number,'TaskUpdate',{'taskId':number,**change})
    user_event([{'type':'tool_result','tool_use_id':'update-'+number,'content':'updated'}])
  if prompt.startswith('subagent-'):
   status=prompt.removeprefix('subagent-')
   emit({'type':'assistant','uuid':'agent-block','session_id':session,'parent_tool_use_id':None,'message':{'id':'agent-message','content':[{'type':'tool_use','id':'agent-call','name':'Agent','input':{'description':'Review source','prompt':'Review the changes','model':'sonnet'}}]}})
   emit({'type':'system','subtype':'task_started','session_id':'foreign','task_id':'foreign-child','tool_use_id':'agent-call'})
   emit({'type':'system','subtype':'task_started','session_id':session,'task_id':'child','tool_use_id':'agent-call','description':'Review source'})
   emit({'type':'system','subtype':'task_notification','session_id':session,'task_id':'child','status':status,'summary':'Observed result'})
   emit({'type':'system','subtype':'task_progress','session_id':session,'task_id':'child'})
  if prompt=='background-shell':
   # Observed from Claude 2.1.296: every Bash run is a task; only a backgrounded one outlives its turn.
   tool_use('bg-call','cargo build');tool_use('fg-call','ls')
   emit({'type':'system','subtype':'task_started','session_id':session,'task_id':'fg-task','tool_use_id':'fg-call','description':'List files','task_type':'local_bash'})
   emit({'type':'system','subtype':'task_started','session_id':session,'task_id':'bg-task','tool_use_id':'bg-call','description':'Watch the build','is_backgrounded':True,'task_type':'local_bash'})
   # The tool's result says where the command writes its output.
   os.makedirs('tasks',exist_ok=True)
   with open('tasks/bg-task.output','w') as output: output.write('Compiling bootty\n')
   user_event([{'type':'tool_result','tool_use_id':'bg-call','content':'Command running in background with ID: bg-task. Output is being written to: '+os.path.abspath('tasks/bg-task.output')}],tool_use_result={'stdout':'','stderr':'','interrupted':False,'backgroundTaskId':'bg-task'})
  if prompt=='background-done':
   emit({'type':'system','subtype':'task_updated','session_id':session,'task_id':'bg-task','patch':{'status':'completed'}})
  if prompt=='self-turn':
   result()
   # Claude reports a finished background task in a turn nobody sent.
   emit({'type':'assistant','uuid':'self-block','session_id':session,'parent_tool_use_id':None,'message':{'id':'self-message','content':[{'type':'text','text':'Background job finished'}]}})
   result()
   continue
  if prompt=='failed': result('model_error','error_during_execution')
  elif prompt=='failed-reason': result('model_error','error_during_execution',errors=['API Error: 529 overloaded'])
  elif prompt=='failed-turns': result('max_turns','error_max_turns')
  else: result()
 elif value['type']=='control_response':
  with open('answer.json','w') as output: json.dump(value,output)
  result()

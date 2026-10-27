#!/usr/bin/env python3
import json,os,socket,sys
if 'exec' in sys.argv:
 assert '--ephemeral' in sys.argv
 with open(sys.argv[sys.argv.index('--output-last-message')+1],'w') as result: json.dump({'title':'Fix native composer','slug':'fix-native-composer'},result)
 sys.exit(0)
assert sys.argv[1:3] == ['app-server','--listen']
source=sys.stdin
output=sys.stdout
if sys.argv[3].startswith('unix://'):
 listener=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM)
 listener.bind(sys.argv[3].removeprefix('unix://'));listener.listen()
 connection,_=listener.accept()
 source=connection.makefile('r');output=connection.makefile('w')
else: assert sys.argv[3]=='stdio://'
thread='thread-'+str(os.getpid())
turn=0
waiting=None
held_start=None
stuck=False
approval=False
methods=[]
turns=[]
reverted=[]
def emit(value): print(json.dumps(value),file=output,flush=True)
def reply(ident,result): emit({'id':ident,'result':result})
def notice(method,**params): emit({'method':method,'params':{'threadId':thread,'turnId':'turn-'+str(turn),**params}})
def tool_call(params):
 config={}
 for index,arg in enumerate(sys.argv):
  if arg=='--config' and index+1<len(sys.argv):
   key,value=sys.argv[index+1].split('=',1)
   if key.startswith('mcp_servers.') and key.endswith('.args'): config=json.loads(value)
 with open(config[1]) as source: connection=json.load(source)
 with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as stream:
  stream.connect(connection['socket'])
  stream.sendall((json.dumps({'token':connection['token'],'request':{'jsonrpc':'2.0','id':1,'method':'tools/call','params':params}})+'\n').encode())
  return json.loads(stream.makefile().readline())['result']
for line in source:
 value=json.loads(line)
 method=value.get('method')
 if method is None:
  # An approval answer finishes the turn that asked for it.
  if approval and 'result' in value: notice('turn/completed',turn={'id':'turn-'+str(turn),'status':'completed','items':[]}); approval=False
  continue
 methods.append(method)
 ident=value.get('id')
 params=value.get('params',{})
 if method == 'initialized': continue
 if method == 'initialize':
  if os.path.exists('fail-initialize'): emit({'id':ident,'error':{'code':-32603,'message':'provider initialization failed'}})
  else: reply(ident,{})
 elif method in ['thread/start','thread/resume']:
  thread=params.get('threadId',thread)
  if method=='thread/start' and os.path.exists('hold-thread-start'):
   held_start=ident
   ready=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM);ready.connect('start-barrier');ready.sendall(b'1');ready.close()
  else: reply(ident,{'thread':{'id':thread,'turns':[]}})
 elif method == '__allow_start':
  reply(held_start,{'thread':{'id':thread,'turns':[]}});held_start=None;reply(ident,{})
 elif method == 'model/list': reply(ident,{'data':[{'model':'qa-model','displayName':'QA Model','isDefault':True,'supportedReasoningEfforts':[{'reasoningEffort':'medium'}],'defaultReasoningEffort':'medium'}],'nextCursor':None})
 elif method == 'thread/fork': reply(ident,{'thread':{'id':'fork-'+str(os.getpid())}})
 elif method == 'thread/items/list': reply(ident,{'data':[],'nextCursor':None})
 elif method == 'thread/turns/list':
  # Newest first, a page at a time; the cursor is where the next page starts.
  start=int(params.get('cursor') or 0);end=start+params['limit']
  page=[{'id':'turn-'+str(n),'items':[{'id':'user-'+str(n),'type':'userMessage','content':[]}]} for n in reversed(turns)][start:end]
  reply(ident,{'data':page,'nextCursor':str(end) if end<len(turns) else None})
 elif method == 'thread/revert':
  before=int(params['beforeTurnId'].removeprefix('turn-'));turns=[n for n in turns if n<before];reverted.append(params['beforeTurnId']);reply(ident,{'thread':{'id':thread}})
 elif method == 'turn/start':
  assert params['threadId'] == thread
  turn+=1
  turns.append(turn)
  text=params['input'][0]['text']
  if text=='visual-image':
   import base64,struct
   assert len(params['input'])==2 and params['input'][1]['type']=='image'
   url=params['input'][1]['url'];assert url.startswith('data:image/png;base64,')
   png=base64.b64decode(url.split(',',1)[1]);assert png[:8]==b'\x89PNG\r\n\x1a\n'
   assert struct.unpack('>II',png[16:24])==(2,2)
  if text=='reject-first':
   emit({'id':ident,'error':{'code':-32600,'message':'first prompt rejected'}});continue
  # The turn edits the conversation's files, as an agent would.
  if text.startswith('write '):
   with open('turn.txt','w') as written: written.write(text[6:]+'\n')
  notice('turn/started',turn={'id':'turn-'+str(turn),'status':'inProgress','items':[]})
  notice('item/completed',item={'id':'user-'+str(turn),'type':'userMessage','content':params['input']})
  if text == 'wait': waiting=ident; continue
  # The turn starts but never reports an end, even after an acknowledged interrupt.
  if text == 'stuck': stuck=True; reply(ident,{'turn':{'id':'turn-'+str(turn)}}); continue
  if text == 'async-question':
   notice('item/completed',item={'id':'question-'+str(turn),'type':'agentMessage','text':'','delivery':'async','questions':[{'title':'Which option?','options':['A','B']}]})
   reply(ident,{'turn':{'id':'turn-'+str(turn)}});continue
  if text == 'approval':
   approval=True
   emit({'id':900,'method':'item/commandExecution/requestApproval','params':{'threadId':thread,'turnId':'turn-'+str(turn),'itemId':'command','command':'echo approved'}})
   reply(ident,{'turn':{'id':'turn-'+str(turn)}});continue
  notice('item/started',item={'id':'command-'+str(turn),'type':'commandExecution','command':'echo probe','aggregatedOutput':''})
  notice('item/commandExecution/outputDelta',itemId='command-'+str(turn),delta='tool output')
  notice('item/agentMessage/delta',itemId='answer-'+str(turn),delta='reply: '+text)
  notice('item/completed',item={'id':'answer-'+str(turn),'type':'agentMessage','text':'reply: '+text})
  notice('turn/completed',turn={'id':'turn-'+str(turn),'status':'completed','items':[]})
  reply(ident,{'turn':{'id':'turn-'+str(turn)}})
 elif method == 'turn/interrupt':
  assert params['threadId'] == thread and params['turnId'] == 'turn-'+str(turn)
  if stuck: reply(ident,{}); continue
  notice('turn/completed',turn={'id':'turn-'+str(turn),'status':'interrupted','items':[]})
  if waiting is not None: reply(waiting,{'turn':{'id':'turn-'+str(turn)}}); waiting=None
  reply(ident,{})
 elif method == '__spawn': reply(ident,tool_call(params))
 elif method == '__barrier': reply(ident,{})
 elif method == '__calls': reply(ident,{'methods':methods,'reverted':reverted,'account_directory':os.environ.get('CODEX_HOME'),'bootty_tools':any(arg.startswith('mcp_servers.') for arg in sys.argv)})
 else: reply(ident,{})

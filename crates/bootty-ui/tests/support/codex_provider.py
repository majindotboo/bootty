#!/usr/bin/env python3
# The executable-replacement cases use a real catalog process.
import json, sys
assert sys.argv[1:4] == ['app-server', '--listen', 'stdio://']
for line in sys.stdin:
    value = json.loads(line)
    if value.get('method') == 'initialized':
        continue
    result = {}
    if value.get('method') == 'model/list':
        result = {'data': [{'model': 'qa-model', 'displayName': 'QA Model',
                           'isDefault': True,
                           'supportedReasoningEfforts': [{'reasoningEffort': 'medium'}],
                           'defaultReasoningEffort': 'medium'}], 'nextCursor': None}
    print(json.dumps({'id': value['id'], 'result': result}), flush=True)

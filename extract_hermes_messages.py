#!/usr/bin/env python3
"""Extract assistant bot messages from Discord HTML exports."""

import os
import re
from collections import defaultdict
from html.parser import HTMLParser

class DiscordMessageParser(HTMLParser):
    def __init__(self):
        super().__init__()
        self.messages = []
        self.current_author = None
        self.current_user_id = None
        self.current_content = []
        self.in_author = False
        self.in_content = False
        self.in_message = False
        self.is_bot = False
        
    def handle_starttag(self, tag, attrs):
        attrs_dict = dict(attrs)
        if tag == 'span' and 'chatlog__author' in attrs_dict.get('class', ''):
            self.in_author = True
            self.current_author = attrs_dict.get('title', '')
            self.current_user_id = attrs_dict.get('data-user-id', '')
            if 'BOT' in attrs_dict.get('class', ''):
                self.is_bot = True
        elif tag == 'span' and 'chatlog__markdown-preserve' in attrs_dict.get('class', ''):
            self.in_content = True
            self.content_start = True
        elif tag == 'div' and 'chatlog__message-container' in attrs_dict.get('class', ''):
            self.in_message = True
            self.msg_id = attrs_dict.get('data-message-id', '')
            
    def handle_endtag(self, tag):
        if tag == 'span' and self.in_content:
            self.in_content = False
        elif tag == 'div' and self.in_message:
            # End of message
            content = ' '.join(self.current_content).strip()
            if self.current_author and content:
                self.messages.append({
                    'author': self.current_author,
                    'user_id': self.current_user_id,
                    'content': content,
                    'is_bot': self.is_bot
                })
            self.current_author = None
            self.current_user_id = None
            self.current_content = []
            self.in_message = False
            self.is_bot = False
            
    def handle_data(self, data):
        if self.in_content:
            self.current_content.append(data)

def extract_messages_from_file(filepath):
    """Extract all messages from a single HTML file."""
    with open(filepath, 'r', encoding='utf-8', errors='ignore') as f:
        content = f.read()
    
    parser = DiscordMessageParser()
    parser.feed(content)
    return parser.messages

def main():
    export_dir = '/root/.hermes/hermes-agent/discord-exports'
    
    # Get all HTML files
    html_files = [f for f in os.listdir(export_dir) if f.endswith('.html')]
    
    print(f"Found {len(html_files)} HTML files\n")
    
    # Extract all messages
    all_messages = []
    for html_file in html_files:
        filepath = os.path.join(export_dir, html_file)
        messages = extract_messages_from_file(filepath)
        for msg in messages:
            msg['source_file'] = html_file
        all_messages.extend(messages)
    
    print(f"Total messages extracted: {len(all_messages)}\n")
    
    # Get unique authors
    authors = set()
    for msg in all_messages:
        authors.add((msg['author'], msg['user_id'], msg['is_bot']))
    
    print("=== UNIQUE AUTHORS ===")
    for author, user_id, is_bot in sorted(authors, key=lambda x: x[0] or 'zzz'):
        print(f"  {author} | {user_id} | bot={is_bot}")
    
    print("\n=== BOT MESSAGES BY AUTHOR ===")
    bot_messages = defaultdict(list)
    for msg in all_messages:
        if msg['is_bot']:
            key = f"{msg['author']} ({msg['user_id']})"
            bot_messages[key].append(msg['content'][:100] + "..." if len(msg['content']) > 100 else msg['content'])
    
    for author, messages in sorted(bot_messages.items()):
        print(f"\n{author}:")
        for m in messages[:5]:  # Preview first 5
            print(f"  - {m}")
        if len(messages) > 5:
            print(f"  ... and {len(messages) - 5} more")

if __name__ == '__main__':
    main()
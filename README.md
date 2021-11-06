Mike's Crawler
==============

Interactive API Docs
--------------------

The running server provides interactive API docs on /swagger/index.html. 

Main Endpoints
--------------

/crawl/{seed} crawls a domain starting with {seed}. 
Returns information about all the links on every page reachable from seed

/crawl/{seed}/list list a domain starting with {seed}.
Returns a list of all the urls that can be found on a domain starting with
seed.

/crawl/{seed}/count count the urls that can be found starting at {seed}

Other Features
--------------

Respects robots.txt on the domain. 
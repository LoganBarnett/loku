module Page.Browse exposing
    ( Model
    , Msg
    , currentLocation
    , init
    , update
    , updateParams
    , view
    )

import Api exposing (DirListing, DiscSetEntry, Entry(..), VideoItem)
import Browser.Navigation as Nav
import Html exposing (..)
import Html.Attributes exposing (..)
import Html.Events exposing (onClick, onInput, onSubmit)
import Http
import Route
import Set exposing (Set)
import Ui


pageSize : Int
pageSize =
    48


type Model
    = Loading Nav.Key Route.BrowseParams
    | Loaded BrowseState
    | Failed String


type alias BrowseState =
    { key : Nav.Key
    , listing : DirListing
    , page : Int
    , query : String
    , expanded : Set String
    }


type Msg
    = GotListing (Result Http.Error DirListing)
    | QueryInput String
    | QuerySubmit
    | GoToPage Int
    | ToggleSet String
    | PickMain String String
    | MainPicked (Result Http.Error ())


init : Nav.Key -> Route.BrowseParams -> ( Model, Cmd Msg )
init key params =
    ( Loading key params
    , Api.getBrowse params.library params.path GotListing
    )


{-| The (library, path) this page is showing, so Main can tell a same-
directory parameter change from a real navigation.
-}
currentLocation : Model -> ( String, String )
currentLocation model =
    case model of
        Loading _ params ->
            ( params.library, params.path )

        Loaded state ->
            ( state.listing.library, state.listing.path )

        Failed _ ->
            ( "", "" )


{-| Update the page number from a new URL without re-fetching the listing.
-}
updateParams : Route.BrowseParams -> Model -> ( Model, Cmd Msg )
updateParams params model =
    case model of
        Loading key existingParams ->
            ( Loading key { existingParams | page = params.page }, Cmd.none )

        Loaded state ->
            ( Loaded { state | page = params.page }, Cmd.none )

        Failed _ ->
            ( model, Cmd.none )


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        GotListing (Ok listing) ->
            case model of
                Loading key params ->
                    ( Loaded
                        { key = key
                        , listing = listing
                        , page = params.page
                        , query = ""
                        , expanded = Set.empty
                        }
                    , Cmd.none
                    )

                Loaded state ->
                    -- A refetch after picking a main title keeps view state.
                    ( Loaded { state | listing = listing }, Cmd.none )

                Failed _ ->
                    ( model, Cmd.none )

        GotListing (Err err) ->
            ( Failed (httpErrorToString err), Cmd.none )

        QueryInput query ->
            case model of
                Loaded state ->
                    ( Loaded { state | query = query }, Cmd.none )

                _ ->
                    ( model, Cmd.none )

        QuerySubmit ->
            case model of
                Loaded state ->
                    if String.isEmpty (String.trim state.query) then
                        ( model, Cmd.none )

                    else
                        ( model
                        , Nav.pushUrl state.key
                            (Route.toString
                                (Route.Search
                                    { query = state.query
                                    , library = Just state.listing.library
                                    , page = 1
                                    }
                                )
                            )
                        )

                _ ->
                    ( model, Cmd.none )

        GoToPage page ->
            case model of
                Loaded state ->
                    ( Loaded { state | page = page }
                    , Nav.pushUrl state.key
                        (Route.toString
                            (Route.Browse
                                { library = state.listing.library
                                , path = state.listing.path
                                , page = page
                                }
                            )
                        )
                    )

                _ ->
                    ( model, Cmd.none )

        ToggleSet discSet ->
            case model of
                Loaded state ->
                    ( Loaded
                        { state
                            | expanded =
                                if Set.member discSet state.expanded then
                                    Set.remove discSet state.expanded

                                else
                                    Set.insert discSet state.expanded
                        }
                    , Cmd.none
                    )

                _ ->
                    ( model, Cmd.none )

        PickMain discSet path ->
            case model of
                Loaded state ->
                    ( model
                    , Api.postMainTitle
                        { library = state.listing.library
                        , discSet = discSet
                        , path = path
                        }
                        MainPicked
                    )

                _ ->
                    ( model, Cmd.none )

        MainPicked (Ok ()) ->
            case model of
                Loaded state ->
                    ( model
                    , Api.getBrowse
                        state.listing.library
                        state.listing.path
                        GotListing
                    )

                _ ->
                    ( model, Cmd.none )

        MainPicked (Err _) ->
            ( model, Cmd.none )


view : Model -> Html Msg
view model =
    case model of
        Loading _ params ->
            p [ class "status-note" ]
                [ text ("Loading " ++ params.library ++ "/" ++ params.path ++ "…") ]

        Failed err ->
            Ui.errorText err

        Loaded state ->
            let
                isDirectory entry =
                    case entry of
                        Directory _ ->
                            True

                        _ ->
                            False

                dirs =
                    state.listing.entries |> List.filter isDirectory

                items =
                    state.listing.entries
                        |> List.filter (not << isDirectory)

                totalPages =
                    Basics.max 1 ((List.length items + pageSize - 1) // pageSize)

                safePage =
                    Basics.min state.page totalPages

                pageEntries =
                    items
                        |> List.drop ((safePage - 1) * pageSize)
                        |> List.take pageSize
            in
            div []
                [ viewBreadcrumb state.listing.library state.listing.path
                , viewSearchBar state.query
                , div [ class "listing" ]
                    (List.map (viewEntry state.listing.library state.expanded) dirs
                        ++ List.map
                            (viewEntry state.listing.library state.expanded)
                            pageEntries
                    )
                , if totalPages > 1 then
                    Ui.viewPagination GoToPage safePage totalPages

                  else
                    text ""
                ]


viewSearchBar : String -> Html Msg
viewSearchBar query =
    Html.form
        [ onSubmit QuerySubmit, class "search-form search-form-bar" ]
        [ input
            [ type_ "search"
            , placeholder "Search this library…"
            , value query
            , onInput QueryInput
            , class "search-input"
            ]
            []
        , button [ type_ "submit", class "search-button" ] [ text "Search" ]
        ]


viewBreadcrumb : String -> String -> Html Msg
viewBreadcrumb library path =
    let
        parts =
            path |> String.split "/" |> List.filter (not << String.isEmpty)

        libraryCrumb =
            span []
                [ text " / "
                , a
                    [ href
                        (Route.toString
                            (Route.Browse
                                { library = library, path = "", page = 1 }
                            )
                        )
                    ]
                    [ text library ]
                ]

        crumbs =
            List.indexedMap
                (\i part ->
                    let
                        crumbPath =
                            parts |> List.take (i + 1) |> String.join "/"
                    in
                    span []
                        [ text " / "
                        , a
                            [ href
                                (Route.toString
                                    (Route.Browse
                                        { library = library
                                        , path = crumbPath
                                        , page = 1
                                        }
                                    )
                                )
                            ]
                            [ text part ]
                        ]
                )
                parts
    in
    nav [ class "crumb-bar" ]
        (a [ href (Route.toString Route.Home) ] [ text "Home" ]
            :: libraryCrumb
            :: crumbs
        )


viewEntry : String -> Set String -> Entry -> Html Msg
viewEntry library expanded entry =
    case entry of
        Directory { name, path } ->
            div [ class "dir-entry" ]
                [ a
                    [ href
                        (Route.toString
                            (Route.Browse
                                { library = library
                                , path = path
                                , page = 1
                                }
                            )
                        )
                    ]
                    [ text ("📁 " ++ name) ]
                ]

        Video item ->
            Ui.videoCard item

        DiscSet set ->
            viewDiscSet (Set.member set.discSet expanded) set


viewDiscSet : Bool -> DiscSetEntry -> Html Msg
viewDiscSet isExpanded set =
    div [ class "video-card" ]
        [ Ui.videoCard set.main
        , div [ class "disc-set-body" ]
            [ button
                [ onClick (ToggleSet set.discSet), class "disc-set-toggle" ]
                [ text
                    ((if isExpanded then
                        "▾ "

                      else
                        "▸ "
                     )
                        ++ String.fromInt (List.length set.titles)
                        ++ " titles"
                    )
                ]
            , if isExpanded then
                div [ class "disc-set-titles" ]
                    (List.map (viewSetTitle set) set.titles)

              else
                text ""
            ]
        ]


viewSetTitle : DiscSetEntry -> VideoItem -> Html Msg
viewSetTitle set item =
    let
        isMain =
            item.path == set.main.path
    in
    div [ class "disc-title-row" ]
        [ a
            [ href
                (Route.toString
                    (Route.Player { library = item.library, path = item.path })
                )
            , class "disc-title-link"
            ]
            [ text
                ((if isMain then
                    "★ "

                  else
                    ""
                 )
                    ++ item.name
                )
            ]
        , if isMain then
            text ""

          else
            button
                [ onClick (PickMain set.discSet item.path)
                , class "disc-main-button"
                ]
                [ text "Set as main" ]
        ]


httpErrorToString : Http.Error -> String
httpErrorToString err =
    case err of
        Http.BadUrl url ->
            "Bad URL: " ++ url

        Http.Timeout ->
            "Request timed out"

        Http.NetworkError ->
            "Network error"

        Http.BadStatus status ->
            "Server error: " ++ String.fromInt status

        Http.BadBody body ->
            "Unexpected response: " ++ body
